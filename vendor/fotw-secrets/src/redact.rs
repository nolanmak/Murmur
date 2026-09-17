//! Never-log enforcement: the last thing between a secret and a log sink.

use std::fmt;
use std::io::{self, Write};
use std::ptr;
use std::sync::atomic::{Ordering, compiler_fence};
use std::sync::{Arc, RwLock};

use crate::{Fingerprint, SecretString};

/// What a stripped credential header is replaced with.
pub const REDACTED: &str = "[REDACTED]";

/// The header names docs/REQUIREMENTS.md 10 requires be stripped before any
/// request or response is logged, in lowercase canonical form.
///
/// `token` is on the list because Deepgram's scheme is
/// `Authorization: Token <key>` and the spec names it separately; treating it
/// as a header name too costs nothing and covers a transport that puts the
/// credential in a header of that name.
pub const SENSITIVE_HEADERS: &[&str] = &["authorization", "xi-api-key", "token", "x-api-key"];

/// Secrets shorter than this are not registerable.
///
/// The empty string is a substring of every string, so registering one would
/// redact the entire log — and an unset environment variable is exactly how an
/// empty `SecretString` arrives. Short strings are barely better: a three-byte
/// "secret" would blank out unrelated words and produce a log so noisy it gets
/// switched off, which protects nothing. No real API key is this short.
const MIN_REGISTERABLE_LEN: usize = 8;

/// Holds the live secrets and rewrites anything containing one.
///
/// # Why this is not a `tracing` layer
///
/// docs/REQUIREMENTS.md 10 asks for "a `tracing` layer". This is the contract
/// that layer would delegate to, implemented standalone, because no crate in
/// this workspace initialises a `tracing` subscriber yet — adding
/// `tracing-subscriber` here to write a `Layer` nobody installs would buy an
/// untested integration and a dependency. [`Redactor::redact_field`] is
/// deliberately shaped like a `Visit::record_str` call, so the eventual
/// `Layer` is a thin adapter over this and the tests below keep testing the
/// part that does the work. [`RedactingWriter`] gives the same guarantee to
/// anything that writes bytes today.
///
/// # Why it holds material, not just fingerprints
///
/// §10 says the layer "holds live key fingerprints". Taken literally that
/// cannot redact: finding a secret *inside* a longer line means substring
/// search, and a fingerprint only answers "is this exact string the secret?".
/// Matching from fingerprints alone means hashing every window of every log
/// line — quadratic, and still only for known lengths. So the registry holds
/// the material in [`SecretString`]s (zeroed on drop, never printed) and the
/// *fingerprints are what appears in the output*: the log says which key it
/// hid, which is the diagnostic value §10 is after, without the key.
///
/// This crate is the right place for that trade — it is already the one place
/// keys live. It would be wrong anywhere else.
pub struct Redactor {
    registry: RwLock<Vec<Registered>>,
}

/// One live secret and the token that replaces it.
struct Registered {
    /// The material to search for.
    material: SecretString,
    /// Identifies the key in the output.
    fingerprint: Fingerprint,
    /// Precomputed `[REDACTED:<fingerprint>]`.
    replacement: String,
}

impl Redactor {
    /// An empty registry. Redacts nothing but still strips credential
    /// headers — those are identified by name, not by contents.
    #[must_use]
    pub fn new() -> Self {
        Self {
            registry: RwLock::new(Vec::new()),
        }
    }

    /// Register a live secret, returning the fingerprint that will stand in
    /// for it.
    ///
    /// Returns `None` — and registers nothing — for material shorter than
    /// [`MIN_REGISTERABLE_LEN`]. Registering the same secret twice is
    /// idempotent so that a re-read of the keychain does not grow the
    /// registry without bound.
    pub fn register(&self, secret: &SecretString) -> Option<Fingerprint> {
        if secret.len() < MIN_REGISTERABLE_LEN {
            return None;
        }
        let fingerprint = Fingerprint::of(secret);
        let mut registry = self.write();
        if !registry.iter().any(|r| r.fingerprint == fingerprint) {
            registry.push(Registered {
                material: secret.clone(),
                replacement: format!("[REDACTED:{fingerprint}]"),
                fingerprint: fingerprint.clone(),
            });
        }
        Some(fingerprint)
    }

    /// Stop redacting a secret — call after rotating a key, so the old one's
    /// material stops being held in memory.
    pub fn forget(&self, fingerprint: &Fingerprint) {
        self.write().retain(|r| &r.fingerprint != fingerprint);
    }

    /// The fingerprints of every live secret. Safe to log; that is the point.
    #[must_use]
    pub fn fingerprints(&self) -> Vec<Fingerprint> {
        self.read().iter().map(|r| r.fingerprint.clone()).collect()
    }

    /// Replace every registered secret in `text` with its fingerprint token.
    ///
    /// Allocates only when there is something to replace, because this sits on
    /// the path of every log line in the process.
    #[must_use]
    pub fn redact(&self, text: &str) -> String {
        let registry = self.read();
        let mut out: Option<String> = None;
        for entry in registry.iter() {
            let haystack = out.as_deref().unwrap_or(text);
            if haystack.contains(entry.material.expose()) {
                out = Some(haystack.replace(entry.material.expose(), &entry.replacement));
            }
        }
        out.unwrap_or_else(|| text.to_owned())
    }

    /// Redact one structured field: a log field, or an HTTP header.
    ///
    /// A credential header is stripped **by name**, whether or not its value
    /// is a registered secret. That matters more than it looks: a bearer token
    /// minted at runtime, or a key the user has typed but not yet saved, is a
    /// credential that the registry has never seen. Name-based stripping is
    /// the only thing that catches those.
    ///
    /// Anything else is still scanned against the registry, because a key can
    /// turn up in a field nobody thought to list.
    #[must_use]
    pub fn redact_field(&self, name: &str, value: &str) -> String {
        if Self::is_sensitive_header(name) {
            return REDACTED.to_owned();
        }
        self.redact(value)
    }

    /// Whether a header name carries credentials and must never be logged.
    #[must_use]
    pub fn is_sensitive_header(name: &str) -> bool {
        SENSITIVE_HEADERS
            .iter()
            .any(|candidate| name.eq_ignore_ascii_case(candidate))
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Vec<Registered>> {
        self.registry.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Vec<Registered>> {
        self.registry.write().unwrap_or_else(|e| e.into_inner())
    }
}

impl Default for Redactor {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Redactor {
    /// Shows how many secrets are live and which, by fingerprint. A derived
    /// `Debug` would print the registry — and the registry is the one place in
    /// this crate that holds every key at once.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Redactor")
            .field("live_secrets", &self.read().len())
            .field("fingerprints", &self.fingerprints())
            .finish()
    }
}

/// A [`Write`] that scrubs everything passing through it.
///
/// Wrapping the *sink* rather than calling [`Redactor::redact`] at each call
/// site is what makes this a control: there is no path where one caller
/// remembers and another forgets. Point it at the log file, and the log file
/// cannot contain a registered secret.
///
/// # Line buffering is load-bearing
///
/// Bytes are held until a newline. A scrub-per-`write` adapter leaks whenever
/// a formatter splits a value across calls — `write!(w, "key={}", secret)` is
/// two `write` calls in most implementations, and neither fragment matches the
/// registry on its own. Buffering to the line boundary reassembles them first.
/// The residual gap is a secret containing a newline; API keys do not, and a
/// multi-line secret (a PEM block, say) would need a different strategy.
pub struct RedactingWriter<W: Write> {
    inner: W,
    redactor: Arc<Redactor>,
    /// Bytes of the current, incomplete line. Holds plaintext, so it is zeroed
    /// after every emit.
    pending: Vec<u8>,
}

impl<W: Write> RedactingWriter<W> {
    /// Wrap `inner`, scrubbing against `redactor`.
    pub fn new(inner: W, redactor: Arc<Redactor>) -> Self {
        Self {
            inner,
            redactor,
            pending: Vec::new(),
        }
    }

    /// Redact one complete line and write it out.
    fn emit(&mut self, line: &[u8]) -> io::Result<()> {
        if line.is_empty() {
            return Ok(());
        }
        // Lossy is correct here: a log sink must not fail on a stray invalid
        // byte, and replacement characters cannot reconstruct a secret.
        let text = String::from_utf8_lossy(line);
        let redacted = self.redactor.redact(&text);
        self.inner.write_all(redacted.as_bytes())
    }

    /// Emit every complete line currently buffered.
    fn drain_lines(&mut self) -> io::Result<()> {
        while let Some(idx) = self.pending.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.pending.drain(..=idx).collect();
            let result = self.emit(&line);
            zero(&mut line);
            result?;
        }
        Ok(())
    }

    /// Emit whatever is buffered, terminated or not.
    fn drain_all(&mut self) -> io::Result<()> {
        self.drain_lines()?;
        if !self.pending.is_empty() {
            let mut tail = std::mem::take(&mut self.pending);
            let result = self.emit(&tail);
            zero(&mut tail);
            result?;
        }
        Ok(())
    }
}

impl<W: Write> Write for RedactingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(buf);
        self.drain_lines()?;
        Ok(buf.len())
    }

    /// Flushes the partial line too.
    ///
    /// An explicit flush means "I want this visible now", and holding back a
    /// half-written line would lose the last message of a crashing process —
    /// which is the message that matters.
    fn flush(&mut self) -> io::Result<()> {
        self.drain_all()?;
        self.inner.flush()
    }
}

impl<W: Write> Drop for RedactingWriter<W> {
    /// Flushes on the way out, so a process that dies mid-line cannot leave a
    /// raw tail in the buffer *or* skip writing it. Errors are swallowed:
    /// there is nobody left to report them to, and panicking in `drop` during
    /// an unwind aborts.
    fn drop(&mut self) {
        let _ = self.drain_all();
        let _ = self.inner.flush();
    }
}

/// Overwrite a buffer that held plaintext.
///
/// Same volatile-write reasoning as [`SecretString::zeroize`].
fn zero(bytes: &mut [u8]) {
    for byte in bytes.iter_mut() {
        // SAFETY: `byte` is a live, uniquely-borrowed, aligned `u8`.
        unsafe { ptr::write_volatile(byte, 0) };
    }
    compiler_fence(Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::sync::Arc;

    use crate::{REDACTED, RedactingWriter, Redactor, SENSITIVE_HEADERS, SecretString};

    const DEEPGRAM_KEY: &str = "dg-0123456789abcdef0123456789abcdef";

    // --------------------------------------------------- registered secrets

    /// The shape this actually happens in: a secret embedded in a longer
    /// line, put there by code that had no idea it was handling a key.
    #[test]
    fn redacts_a_registered_secret_embedded_in_a_line() {
        let redactor = Redactor::new();
        let fp = redactor.register(&SecretString::new(DEEPGRAM_KEY)).unwrap();

        let line =
            format!("connecting to wss://api.deepgram.com token={DEEPGRAM_KEY} model=nova-3");
        let out = redactor.redact(&line);

        assert!(
            !out.contains(DEEPGRAM_KEY),
            "secret survived redaction: {out}"
        );
        assert!(
            out.contains("wss://api.deepgram.com"),
            "over-redacted: {out}"
        );
        assert!(out.contains("model=nova-3"), "over-redacted: {out}");
        assert!(
            out.contains(fp.as_str()),
            "redaction should name which key it hid: {out}"
        );
    }

    /// The test that keeps the redactor honest. If `redact` were a blanket
    /// "replace everything that looks like a key", this would pass for the
    /// wrong reason and the tests above would prove nothing about matching.
    #[test]
    fn leaves_unregistered_text_alone() {
        let redactor = Redactor::new();
        redactor.register(&SecretString::new(DEEPGRAM_KEY)).unwrap();

        let line = "an ordinary log line about sk-not-a-registered-secret and nothing else";
        assert_eq!(redactor.redact(line), line);
    }

    #[test]
    fn redacts_every_registered_secret_in_one_pass() {
        let redactor = Redactor::new();
        redactor
            .register(&SecretString::new("secret-alpha-aaaaaaaa"))
            .unwrap();
        redactor
            .register(&SecretString::new("secret-bravo-bbbbbbbb"))
            .unwrap();

        let out = redactor.redact("alpha=secret-alpha-aaaaaaaa bravo=secret-bravo-bbbbbbbb");
        assert!(!out.contains("secret-alpha-aaaaaaaa"), "{out}");
        assert!(!out.contains("secret-bravo-bbbbbbbb"), "{out}");
        assert_eq!(redactor.fingerprints().len(), 2);
    }

    #[test]
    fn forgetting_a_rotated_key_stops_redacting_it() {
        let redactor = Redactor::new();
        let fp = redactor.register(&SecretString::new(DEEPGRAM_KEY)).unwrap();
        assert!(!redactor.redact(DEEPGRAM_KEY).contains(DEEPGRAM_KEY));

        redactor.forget(&fp);
        assert!(redactor.fingerprints().is_empty());
        assert_eq!(redactor.redact(DEEPGRAM_KEY), DEEPGRAM_KEY);
    }

    /// The empty string is a substring of every string, so registering one
    /// would redact the entire log. Refusing is not pedantry — a
    /// `SecretString` built from an unset environment variable is exactly how
    /// this arrives.
    #[test]
    fn refuses_to_register_an_empty_or_trivially_short_secret() {
        let redactor = Redactor::new();
        assert!(redactor.register(&SecretString::new("")).is_none());
        assert!(redactor.register(&SecretString::new("ab")).is_none());
        assert!(redactor.fingerprints().is_empty());
        assert_eq!(
            redactor.redact("nothing here is secret"),
            "nothing here is secret"
        );
    }

    #[test]
    fn registering_the_same_secret_twice_is_idempotent() {
        let redactor = Redactor::new();
        let first = redactor.register(&SecretString::new(DEEPGRAM_KEY)).unwrap();
        let second = redactor.register(&SecretString::new(DEEPGRAM_KEY)).unwrap();
        assert_eq!(first, second);
        assert_eq!(redactor.fingerprints().len(), 1);
    }

    // ------------------------------------------------------------- headers

    /// docs/REQUIREMENTS.md 10: "The HTTP wrapper strips `Authorization`,
    /// `xi-api-key`, `Token`, `x-api-key` before any request/response is
    /// logged."
    ///
    /// Crucially this must fire for values that are **not** registered
    /// secrets. A bearer token minted at runtime, a key the user just typed
    /// and has not saved yet, a provider's session token — none of those are
    /// in the registry, and all of them are credentials.
    #[test]
    fn strips_the_four_credential_headers_even_when_unregistered() {
        let redactor = Redactor::new();
        assert!(redactor.fingerprints().is_empty(), "registry must be empty");

        for name in SENSITIVE_HEADERS {
            let value = "Bearer never-registered-anywhere-12345";
            let out = redactor.redact_field(name, value);
            assert_eq!(out, REDACTED, "header {name} was not stripped");
        }
    }

    #[test]
    fn header_matching_is_case_insensitive() {
        let redactor = Redactor::new();
        for name in [
            "Authorization",
            "AUTHORIZATION",
            "authorization",
            "Token",
            "X-Api-Key",
        ] {
            assert_eq!(
                redactor.redact_field(name, "Token abcdef"),
                REDACTED,
                "header {name} was not stripped"
            );
        }
        assert!(Redactor::is_sensitive_header("xi-api-key"));
        assert!(!Redactor::is_sensitive_header("content-type"));
    }

    /// Over-redaction is a real cost: a log where everything is `[REDACTED]`
    /// gets turned off, and a log that is off protects nothing.
    #[test]
    fn ordinary_headers_pass_through_but_still_get_scanned() {
        let redactor = Redactor::new();
        redactor.register(&SecretString::new(DEEPGRAM_KEY)).unwrap();

        assert_eq!(
            redactor.redact_field("content-type", "application/json"),
            "application/json"
        );

        // ...and a secret that turns up in a header nobody listed is still
        // caught by the registry.
        let out = redactor.redact_field("x-custom-debug", &format!("key={DEEPGRAM_KEY}"));
        assert!(!out.contains(DEEPGRAM_KEY), "{out}");
    }

    // ------------------------------------------------------- writer adapter

    /// The adapter that makes redaction a property of the *sink*. Anything
    /// written through it is scanned; there is no path where a caller
    /// remembers to call `redact` and another forgets.
    #[test]
    fn redacting_writer_scrubs_what_is_written_through_it() {
        let redactor = Arc::new(Redactor::new());
        redactor.register(&SecretString::new(DEEPGRAM_KEY)).unwrap();

        let mut sink = Vec::new();
        {
            let mut writer = RedactingWriter::new(&mut sink, Arc::clone(&redactor));
            writeln!(writer, "starting session").unwrap();
            writeln!(writer, "Authorization: Token {DEEPGRAM_KEY}").unwrap();
            writeln!(writer, "done").unwrap();
            writer.flush().unwrap();
        }

        let text = String::from_utf8(sink).unwrap();
        assert!(
            !text.contains(DEEPGRAM_KEY),
            "writer leaked the key: {text}"
        );
        assert!(text.contains("starting session"), "{text}");
        assert!(text.contains("done"), "{text}");
        assert!(
            text.contains("[REDACTED"),
            "nothing was marked as redacted: {text}"
        );
    }

    /// A secret split across two `write` calls is the way a naive
    /// scrub-per-call adapter leaks. Buffering to the line boundary is what
    /// closes it.
    #[test]
    fn redacting_writer_holds_partial_lines_until_the_newline() {
        let redactor = Arc::new(Redactor::new());
        redactor.register(&SecretString::new(DEEPGRAM_KEY)).unwrap();

        let (head, tail) = DEEPGRAM_KEY.split_at(10);
        let mut sink = Vec::new();
        {
            let mut writer = RedactingWriter::new(&mut sink, redactor);
            writer.write_all(b"key=").unwrap();
            writer.write_all(head.as_bytes()).unwrap();
            // Nothing may have reached the sink yet — the line is incomplete.
            writer.write_all(tail.as_bytes()).unwrap();
            writer.write_all(b"\n").unwrap();
            writer.flush().unwrap();
        }

        let text = String::from_utf8(sink).unwrap();
        assert!(
            !text.contains(DEEPGRAM_KEY),
            "split write leaked the key: {text}"
        );
        assert!(
            !text.contains(head),
            "split write leaked a key prefix: {text}"
        );
    }

    /// A process that dies without a trailing newline must not flush the raw
    /// tail. Drop redacts what is pending.
    #[test]
    fn redacting_writer_redacts_the_unterminated_tail_on_drop() {
        let redactor = Arc::new(Redactor::new());
        redactor.register(&SecretString::new(DEEPGRAM_KEY)).unwrap();

        let mut sink = Vec::new();
        {
            let mut writer = RedactingWriter::new(&mut sink, redactor);
            write!(writer, "trailing key={DEEPGRAM_KEY}").unwrap();
        } // dropped without a newline and without an explicit flush

        let text = String::from_utf8(sink).unwrap();
        assert!(!text.contains(DEEPGRAM_KEY), "drop leaked the key: {text}");
        assert!(
            text.contains("trailing key="),
            "drop dropped the line: {text}"
        );
    }

    /// The redactor is shared by every log sink in the process, so it has to
    /// be usable from `&` across threads.
    #[test]
    fn redactor_is_shareable_across_threads() {
        let redactor = Arc::new(Redactor::new());
        redactor.register(&SecretString::new(DEEPGRAM_KEY)).unwrap();

        let handles: Vec<_> = (0..4)
            .map(|_| {
                let redactor = Arc::clone(&redactor);
                std::thread::spawn(move || {
                    let out = redactor.redact(&format!("k={DEEPGRAM_KEY}"));
                    assert!(!out.contains(DEEPGRAM_KEY));
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
    }

    /// The redactor is itself a holder of key material, so it owes the same
    /// guarantee `SecretString` does.
    #[test]
    fn redactor_debug_never_prints_the_registry() {
        let redactor = Redactor::new();
        redactor.register(&SecretString::new(DEEPGRAM_KEY)).unwrap();
        let debug = format!("{redactor:?}");
        assert!(
            !debug.contains(DEEPGRAM_KEY),
            "Redactor Debug leaked: {debug}"
        );
    }
}
