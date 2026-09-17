//! The only type allowed to hold key material.

use std::fmt;
use std::hint::black_box;
use std::ptr;
use std::sync::atomic::{Ordering, compiler_fence};

/// A string that is known to be secret.
///
/// Three properties, each of which is a control from docs/REQUIREMENTS.md 10
/// rather than a nicety:
///
/// 1. **`Debug` and `Display` redact.** This is the one that matters. Keys do
///    not leak through code that handles keys — that code is reviewed. They
///    leak through `tracing::debug!(?config)` on a struct three layers up that
///    happens to contain one. Making the redaction a property of the *type*
///    means every containing struct's derived `Debug` inherits it.
/// 2. **No `Deref<Target = str>`.** Deref would make a `SecretString` usable
///    anywhere a `&str` is, which is convenient and which is exactly the
///    problem: it would make the use sites invisible. The material comes out
///    of [`SecretString::expose`] and nowhere else, so `rg 'expose\('` is a
///    complete audit of everywhere a key is in the clear.
/// 3. **Zeroed on drop.** Best-effort — see [`SecretString::zeroize`].
///
/// # Why not `zeroize`
///
/// The `zeroize` crate is the usual answer and is a fine crate. We hand-roll
/// the ~10 lines instead because taking a dependency buys us a derive macro we
/// would not use and a trait hierarchy we would not implement, on a crate
/// whose entire job here is one volatile write loop. The trade is a comment
/// explaining the memory model (below) against a supply-chain edge; for this
/// much code the comment wins.
pub struct SecretString {
    /// Invariant: only ever mutated through [`SecretString::zeroize`], which
    /// preserves UTF-8 (it writes NUL, which is valid, then truncates).
    inner: String,
}

impl SecretString {
    /// Wrap key material.
    ///
    /// Takes ownership. Note the limit of that: if the caller built the
    /// `String` by concatenation, intermediate allocations already holding the
    /// material are outside our reach. Read keys straight into a
    /// `SecretString` where you can.
    pub fn new(material: impl Into<String>) -> Self {
        Self {
            inner: material.into(),
        }
    }

    /// Wrap material that came from a human pasting into a text field,
    /// trimming surrounding whitespace.
    ///
    /// Copying a key out of a provider dashboard reliably brings a trailing
    /// newline, and a key that fails validation because of an invisible
    /// character produces a support ticket that is impossible to diagnose from
    /// the user's side — they can *see* that they pasted the right key.
    pub fn from_pasted(material: impl AsRef<str>) -> Self {
        Self::new(material.as_ref().trim())
    }

    /// The key material, in the clear.
    ///
    /// **Every call site is a place a secret can escape.** That is the point:
    /// this method is deliberately the only door, and deliberately named
    /// something greppable. Do not store the returned `&str` anywhere that
    /// outlives the call — hand it to the transport and let it go.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.inner
    }

    /// Length of the material in bytes.
    ///
    /// Safe to log: it distinguishes "empty" from "present" and "present" from
    /// "present with a trailing newline", which is most of what a log line
    /// needs to say about a key.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// True when there is no material at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Overwrite the buffer with zeroes and truncate it.
    ///
    /// Called automatically on drop. Explicit calls are for the case where a
    /// secret has to die before its scope ends.
    ///
    /// # What this does and does not guarantee
    ///
    /// [`ptr::write_volatile`] is the guarantee that the compiler will not
    /// elide the stores as dead — a plain `for b in .. { *b = 0 }` on a buffer
    /// about to be freed is exactly the write LLVM is licensed to delete. The
    /// [`compiler_fence`] keeps the stores from being reordered past the
    /// deallocation that follows.
    ///
    /// What it cannot do is chase copies. If the allocator moved the buffer
    /// during a `String` growth, or the OS paged it to swap, or a `memcpy`
    /// left a fragment on the stack, those copies survive. This raises the
    /// cost of reading a key out of a core dump; it does not make it
    /// impossible, and docs/REQUIREMENTS.md 10.1 already puts same-user local
    /// malware out of scope.
    pub fn zeroize(&mut self) {
        // SAFETY: every byte is overwritten with 0x00 and the buffer is then
        // truncated to zero length. NUL is valid UTF-8, so `String`'s
        // invariant holds at every point at which an unwind could observe the
        // buffer, and the empty string trivially satisfies it afterwards.
        let bytes = unsafe { self.inner.as_mut_vec() };
        for byte in bytes.iter_mut() {
            // SAFETY: `byte` is a live, uniquely-borrowed, aligned `u8`.
            unsafe { ptr::write_volatile(byte, 0) };
        }
        compiler_fence(Ordering::SeqCst);
        bytes.clear();
    }

    /// Compare two secrets without short-circuiting on the first differing
    /// byte.
    ///
    /// A plain `==` over key material is a timing oracle: an attacker who can
    /// submit candidate keys and time the comparison recovers the key byte by
    /// byte. This is why [`SecretString`] does not implement `PartialEq` —
    /// there is no safe default, so there is no default.
    ///
    /// **Length is not hidden.** Unequal lengths return early. Hiding length
    /// would mean hashing both sides, and for our use (comparing a key against
    /// itself across a store round-trip) the length is not the secret.
    #[must_use]
    pub fn ct_eq(&self, other: &Self) -> bool {
        let (a, b) = (self.inner.as_bytes(), other.inner.as_bytes());
        if a.len() != b.len() {
            return false;
        }
        let mut diff = 0u8;
        for (x, y) in a.iter().zip(b) {
            diff |= x ^ y;
        }
        // `black_box` stops the optimiser from turning the accumulate-then-test
        // back into an early-exit loop.
        black_box(diff) == 0
    }
}

/// The text substituted for key material in `Debug` and `Display`.
const REDACTION: &str = "redacted";

impl fmt::Debug for SecretString {
    /// Prints `<redacted:NB>`, never the material.
    ///
    /// The byte count is deliberate: a redaction that erased *everything*
    /// leaves an operator with no way to tell "the key is empty" from "the key
    /// is fine", and an operator who cannot tell reaches for `expose()` to
    /// write the log line — which puts the key in the log. Publishing the
    /// length buys enough diagnostic value to remove that temptation.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{REDACTION}:{}B>", self.inner.len())
    }
}

impl fmt::Display for SecretString {
    /// Identical to [`Debug`](fmt::Debug). `Display` is what `{}` reaches for
    /// and `{}` is what a hurried log line uses, so it cannot be the lenient
    /// one.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{REDACTION}:{}B>", self.inner.len())
    }
}

impl Drop for SecretString {
    fn drop(&mut self) {
        self.zeroize();
    }
}

impl Clone for SecretString {
    /// Hand-written rather than derived so the cost is stated: a clone is a
    /// second copy of the material with its own lifetime, and therefore a
    /// second chance to leak. Clone when the store has to hand a key to two
    /// transports; do not clone to dodge a borrow.
    fn clone(&self) -> Self {
        Self::new(self.inner.clone())
    }
}

#[cfg(test)]
mod tests {
    use crate::SecretString;

    /// The whole failure mode in one test: a key that reaches a log line
    /// through a `{:?}` in a struct nobody thought about.
    ///
    /// This asserts the *absence* of the material, not the presence of a
    /// marker, because a `Debug` that printed `SecretString("sk-live-…")`
    /// would still contain the marker.
    #[test]
    fn debug_and_display_never_print_the_material() {
        let secret = SecretString::new("sk-live-DO-NOT-LOG-ME");

        let debug = format!("{secret:?}");
        let display = format!("{secret}");

        assert!(
            !debug.contains("sk-live-DO-NOT-LOG-ME"),
            "Debug leaked: {debug}"
        );
        assert!(
            !display.contains("sk-live-DO-NOT-LOG-ME"),
            "Display leaked: {display}"
        );
        assert!(
            !debug.contains("DO-NOT-LOG-ME"),
            "Debug leaked a substring: {debug}"
        );
    }

    /// A `SecretString` nested inside a derived-`Debug` struct is the shape
    /// that actually leaks in practice: nobody writes `{:?}` on the key, they
    /// write it on the config struct that happens to contain it.
    #[test]
    fn debug_is_redacted_when_nested_in_a_derived_debug_struct() {
        #[derive(Debug)]
        #[allow(dead_code)]
        struct ProviderConfig {
            endpoint: &'static str,
            key: SecretString,
        }

        let cfg = ProviderConfig {
            endpoint: "https://api.deepgram.com",
            key: SecretString::new("dg-secret-value"),
        };

        let rendered = format!("{cfg:?}");
        assert!(!rendered.contains("dg-secret-value"), "leaked: {rendered}");
        assert!(
            rendered.contains("api.deepgram.com"),
            "over-redacted: {rendered}"
        );
    }

    /// The redaction has to say *something* useful, or callers will reach for
    /// `expose()` just to write a log line — which is how the material ends up
    /// in the log anyway. Length is safe to publish and is enough to tell
    /// "empty string" from "someone pasted a key with a trailing newline".
    #[test]
    fn redaction_reports_length_but_not_content() {
        let secret = SecretString::new("0123456789");
        let debug = format!("{secret:?}");
        assert!(debug.contains("10"), "no length hint in {debug}");
        assert!(!debug.contains("0123456789"));
    }

    #[test]
    fn expose_returns_the_material() {
        let secret = SecretString::new("sk-abc");
        assert_eq!(secret.expose(), "sk-abc");
        assert_eq!(secret.len(), 6);
        assert!(!secret.is_empty());
    }

    /// Zeroing on drop. We cannot observe a freed allocation without UB, so
    /// this drives the same volatile-write path over a buffer we still own and
    /// asserts the bytes are gone.
    #[test]
    fn zeroize_clears_the_buffer() {
        let mut secret = SecretString::new("sk-live-aaaaaaaaaaaa");
        let before = secret.expose().to_owned();
        assert!(before.contains("aaaa"));

        secret.zeroize();

        assert_eq!(secret.expose(), "", "buffer not cleared");
        assert_eq!(secret.len(), 0);
    }

    /// Comparison must not be a plain `==` on the material, because a
    /// short-circuiting compare over a secret is a timing oracle. This only
    /// asserts the contract; it cannot assert the timing.
    #[test]
    fn constant_time_eq_matches_value_equality() {
        let a = SecretString::new("sk-identical");
        let b = SecretString::new("sk-identical");
        let c = SecretString::new("sk-different");
        let d = SecretString::new("sk-identical-but-longer");

        assert!(a.ct_eq(&b));
        assert!(!a.ct_eq(&c));
        assert!(!a.ct_eq(&d));
    }

    #[test]
    fn trims_surrounding_whitespace_on_paste() {
        // Pasting from a provider dashboard reliably brings a trailing
        // newline. A key that fails to validate because of an invisible
        // character is a support ticket we can delete in one line here.
        let secret = SecretString::from_pasted("  sk-abc\n");
        assert_eq!(secret.expose(), "sk-abc");
    }

    #[test]
    fn clone_is_independent() {
        let a = SecretString::new("sk-abc");
        let mut b = a.clone();
        b.zeroize();
        assert_eq!(a.expose(), "sk-abc", "clone shared the original's buffer");
    }
}
