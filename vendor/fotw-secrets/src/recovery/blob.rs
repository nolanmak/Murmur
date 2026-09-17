//! The sealed master key, and the file it lives in.
//!
//! # The construction
//!
//! ```text
//!   KEK   = Argon2id(pw = recovery key bytes, salt = 16 random bytes,
//!                    m = 64 MiB, t = 3, p = 1, out = 32)
//!   AAD   = "fotw-recovery-v1\0" || le32(m) || le32(t) || le32(p) || salt
//!   blob  = XChaCha20-Poly1305-seal(KEK, nonce = 24 random bytes, AAD,
//!                                   msg = master key)
//! ```
//!
//! # Argon2id, and why these numbers
//!
//! The threat is stated in [`super`]: **an offline attacker who has the file.**
//! That is not a hypothetical, it is the design — the blob has to live outside
//! the keychain to be useful, so it travels in every backup the database does.
//!
//! *Why a memory-hard KDF when the input already has 128 bits of entropy.*
//! Honestly: against a full-entropy Recovery Key, Argon2 buys nothing. 2¹²⁸ is
//! out of reach whether each guess costs a nanosecond or a second, and it would
//! be dishonest to present the KDF as what makes this safe — the entropy is.
//! Argon2id earns its place against the cases where the input is *not* full
//! entropy, and those are the realistic ones:
//!
//! * **Partial disclosure.** A photograph of the card with a thumb over two
//!   groups, a shoulder-surfed screen, a key read aloud on a call with the last
//!   groups missed. Eight characters missing leaves 40 bits, and 40 bits at
//!   SHA-speed is minutes on a laptop. At 64 MiB × 3 passes it is centuries of
//!   GPU time, because the memory, not the arithmetic, is the wall.
//! * **A future passphrase option.** The moment anyone lets a user supply their
//!   own recovery phrase — and users will ask — the entropy assumption is gone
//!   and only the KDF stands between the blob and a wordlist.
//!
//! *Why 64 MiB, t = 3.* RFC 9106's second recommended option exactly
//! (§4: "If much less memory is available, use t = 3, m = 2¹⁶ KiB, p = 4").
//! 64 MiB is a rounding error on any machine that can run a meeting recorder,
//! and it is the point where a GPU's memory bandwidth stops being an advantage.
//! Measured here at roughly 100–200 ms, paid **once**, on a path a user takes
//! at most a handful of times in the life of an install. This is not the
//! per-connection `PRAGMA key` cost that §9.1 goes out of its way to avoid —
//! that is the master key's job and it is still a raw key.
//!
//! *Why p = 1 and not RFC 9106's p = 4.* Because the `argon2` crate computes
//! lanes sequentially unless its `parallel` (rayon) feature is on. With p = 4
//! and one thread we would pay four times the wall-clock for the *same* memory
//! footprint, while an attacker with four cores gets the parallelism for free —
//! strictly worse for us on both sides of the trade. p = 1 with the memory held
//! constant is the honest single-threaded configuration.
//!
//! The parameters are stored in the file, so raising them later is a matter of
//! rewriting one blob, and old blobs keep opening.
//!
//! # XChaCha20-Poly1305
//!
//! The extended-nonce variant, so a 24-byte nonce can come straight from the
//! CSPRNG: at 2⁻⁹⁶ collision probability there is no counter to persist, and
//! therefore no counter for an attacker to roll back by restoring an older copy
//! of the file. The AAD binds the version, the Argon2 parameters and the salt,
//! so an edit to any of them is a tag failure rather than a silent derivation
//! of some other key.
//!
//! # The integrity digest, and what it is *not*
//!
//! The file also carries `integrity`: eight bytes of SHA-256 over the AAD, the
//! nonce and the sealed bytes. It is **not** a security control — anyone who
//! edits the file can recompute it — and it deliberately does not cover any
//! secret.
//!
//! It exists for one reason: an AEAD tag failure cannot tell "you typed the
//! wrong Recovery Key" apart from "this file got damaged", and those two send a
//! user to completely different places. With a key-independent digest, damage
//! is caught *before* the tag is ever checked, so the wrong-key error is
//! reserved for the case where the file is intact and the key simply is not the
//! one. That distinction is the whole point of [`RecoveryError`].
//!
//! # What is deliberately absent
//!
//! There is **no fingerprint of the Recovery Key** in the file, and there must
//! never be one. It would be the obvious way to say "wrong key" quickly, and it
//! would hand an offline attacker a SHA-256-speed oracle for testing guesses —
//! collapsing the Argon2 cost to nothing and undoing the entire argument above.
//! The tag is the check. It is slow on purpose.

use std::path::{Path, PathBuf};

use chacha20poly1305::aead::{Aead, Generate, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use sha2::{Digest, Sha256};

use super::{MasterKeyBytes, RecoveryError, RecoveryKey, zeroize_bytes};

/// Format version, and the first bytes of the AAD.
const FORMAT_TAG: &[u8] = b"fotw-recovery-v1\0";

/// The `fotw_recovery` field's value.
const FORMAT_VERSION: u32 = 1;

const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
/// 32-byte master key + 16-byte Poly1305 tag.
const SEALED_LEN: usize = 48;
const INTEGRITY_LEN: usize = 8;

/// What the file says about itself, for whoever finds it in a backup.
const NOTE: &str = "This is NOT your Recovery Key. It holds the FlyOnTheWall database \
                    master key sealed with Argon2id + XChaCha20-Poly1305, and cannot open \
                    anything on its own -- you need the Recovery Key you wrote down at \
                    first run. Keep this file with db.sqlite3; losing both is permanent \
                    data loss. See docs/REQUIREMENTS.md section 10.";

/// Argon2id cost parameters, stored alongside the blob.
///
/// Stored rather than compiled in, so that raising the cost later is a rewrite
/// of one file instead of a flag day that orphans every existing Recovery Key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KdfParams {
    /// Memory cost, in KiB.
    pub m_cost_kib: u32,
    /// Time cost: passes over that memory.
    pub t_cost: u32,
    /// Parallelism: lanes. See the module docs for why this is 1.
    pub p_cost: u32,
}

impl Default for KdfParams {
    /// RFC 9106's second recommended option, with `p` corrected for a
    /// single-threaded build. See the module docs.
    fn default() -> Self {
        Self {
            m_cost_kib: 65_536,
            t_cost: 3,
            p_cost: 1,
        }
    }
}

/// A 32-byte master key sealed under a Recovery Key.
///
/// `PartialEq` compares ciphertext and public parameters only — there is no
/// secret in this type, which is the entire reason it is allowed on disk.
#[derive(Clone, PartialEq, Eq)]
pub struct WrappedMasterKey {
    kdf: KdfParams,
    salt: [u8; SALT_LEN],
    nonce: [u8; NONCE_LEN],
    sealed: [u8; SEALED_LEN],
}

impl std::fmt::Debug for WrappedMasterKey {
    /// Prints the parameters and not the ciphertext.
    ///
    /// The ciphertext is not secret, but it is the input an attacker needs, and
    /// a log line is the one place it should never end up being copied out of
    /// by accident.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WrappedMasterKey")
            .field("kdf", &self.kdf)
            .field("sealed", &"<48 bytes>")
            .finish()
    }
}

impl WrappedMasterKey {
    /// Seal `master` under `recovery`.
    ///
    /// Draws a fresh salt and nonce every call, so wrapping the same pair twice
    /// produces different blobs and the file never reveals that two libraries
    /// share a key.
    ///
    /// # Errors
    ///
    /// [`RecoveryError::Crypto`] if the OS CSPRNG fails or the parameters are
    /// ones Argon2 rejects.
    pub fn wrap(
        master: &MasterKeyBytes,
        recovery: &RecoveryKey,
        kdf: KdfParams,
    ) -> Result<Self, RecoveryError> {
        let salt: [u8; SALT_LEN] = os_random()?;
        let nonce: [u8; NONCE_LEN] = os_random()?;

        let mut kek = derive_kek(recovery, &salt, kdf)?;
        let cipher = XChaCha20Poly1305::new((&kek).into());
        zeroize_bytes(&mut kek);

        let aad = aad(kdf, &salt);
        let out = cipher
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: master.expose(),
                    aad: &aad,
                },
            )
            .map_err(|_| RecoveryError::Crypto("sealing the master key failed".to_owned()))?;

        let sealed: [u8; SEALED_LEN] = out.as_slice().try_into().map_err(|_| {
            RecoveryError::Crypto(format!("sealed {} bytes, expected {SEALED_LEN}", out.len()))
        })?;

        Ok(Self {
            kdf,
            salt,
            nonce,
            sealed,
        })
    }

    /// Open the seal.
    ///
    /// `path` is used only to name the file in error messages; a user told
    /// "that key does not open this library" needs to know *which* library.
    ///
    /// # Errors
    ///
    /// [`RecoveryError::WrongRecoveryKey`] when the tag does not verify — a
    /// well-formed key that is not this one, or a file that has been edited.
    /// [`RecoveryError::Crypto`] for a KDF failure. Never anything that reads
    /// as database corruption, because the database has not been touched.
    pub fn unwrap_master(
        &self,
        recovery: &RecoveryKey,
        path: &Path,
    ) -> Result<MasterKeyBytes, RecoveryError> {
        let mut kek = derive_kek(recovery, &self.salt, self.kdf)?;
        let cipher = XChaCha20Poly1305::new((&kek).into());
        zeroize_bytes(&mut kek);

        let aad = aad(self.kdf, &self.salt);
        let mut opened = cipher
            .decrypt(
                &XNonce::from(self.nonce),
                Payload {
                    msg: &self.sealed,
                    aad: &aad,
                },
            )
            .map_err(|_| RecoveryError::WrongRecoveryKey {
                path: path.to_path_buf(),
            })?;

        let key = MasterKeyBytes::from_slice(&opened);
        // The plaintext left the AEAD in a plain `Vec`, which is outside
        // `MasterKeyBytes`'s protection. Wipe it before it is dropped and freed.
        zeroize_bytes(&mut opened);
        key
    }

    /// The parameters this blob was sealed with.
    #[must_use]
    pub fn kdf(&self) -> KdfParams {
        self.kdf
    }

    /// Serialise to the on-disk JSON form.
    ///
    /// Hand-rolled rather than `serde_json`, for two reasons that both matter
    /// here. The field order is part of the file's readability — the note has
    /// to come first, or nobody reads it — and this crate should not grow a
    /// runtime JSON dependency for one 200-byte document. Every value is hex or
    /// an integer, so there is nothing to escape.
    #[must_use]
    pub fn to_json(&self) -> String {
        format!(
            "{{\n  \
             \"fotw_recovery\": {FORMAT_VERSION},\n  \
             \"note\": \"{NOTE}\",\n  \
             \"kdf\": \"argon2id\",\n  \
             \"m_cost_kib\": {},\n  \
             \"t_cost\": {},\n  \
             \"p_cost\": {},\n  \
             \"cipher\": \"xchacha20poly1305\",\n  \
             \"salt\": \"{}\",\n  \
             \"nonce\": \"{}\",\n  \
             \"sealed_key\": \"{}\",\n  \
             \"integrity\": \"{}\"\n\
             }}\n",
            self.kdf.m_cost_kib,
            self.kdf.t_cost,
            self.kdf.p_cost,
            hex(&self.salt),
            hex(&self.nonce),
            hex(&self.sealed),
            hex(&self.integrity()),
        )
    }

    /// Parse the on-disk JSON form.
    ///
    /// Every failure here is [`RecoveryError::CorruptBlob`], never a wrong-key
    /// error: at this point no key has been offered, so blaming one would be a
    /// lie that costs the user their next hour.
    ///
    /// # Errors
    ///
    /// [`RecoveryError::CorruptBlob`] for a missing field, a bad hex value, a
    /// wrong field length, an unknown version, or a failed integrity digest.
    pub fn from_json(text: &str, path: &Path) -> Result<Self, RecoveryError> {
        let bad = |detail: String| RecoveryError::CorruptBlob {
            path: path.to_path_buf(),
            detail,
        };

        let version: u32 = number(text, "fotw_recovery")
            .ok_or_else(|| bad("not a FlyOnTheWall recovery file".to_owned()))?;
        if version != FORMAT_VERSION {
            return Err(bad(format!(
                "it is format version {version} and this build understands {FORMAT_VERSION}; \
                 upgrade FlyOnTheWall rather than editing the file"
            )));
        }

        let kdf = KdfParams {
            m_cost_kib: number(text, "m_cost_kib")
                .ok_or_else(|| bad("no m_cost_kib".to_owned()))?,
            t_cost: number(text, "t_cost").ok_or_else(|| bad("no t_cost".to_owned()))?,
            p_cost: number(text, "p_cost").ok_or_else(|| bad("no p_cost".to_owned()))?,
        };

        let salt = fixed_hex::<SALT_LEN>(text, "salt").map_err(&bad)?;
        let nonce = fixed_hex::<NONCE_LEN>(text, "nonce").map_err(&bad)?;
        let sealed = fixed_hex::<SEALED_LEN>(text, "sealed_key").map_err(&bad)?;
        let stored = fixed_hex::<INTEGRITY_LEN>(text, "integrity").map_err(&bad)?;

        let blob = Self {
            kdf,
            salt,
            nonce,
            sealed,
        };
        if blob.integrity() != stored {
            return Err(bad(
                "its integrity check does not match, so the file has been changed or \
                 damaged since it was written. Restore it from the same backup as \
                 db.sqlite3 — this is a problem with this file, not with your \
                 Recovery Key and not with your database"
                    .to_owned(),
            ));
        }
        Ok(blob)
    }

    /// Write to `path`, mode 0600, atomically.
    ///
    /// Temp file plus `rename(2)`, per ING-12's reasoning applied here: a crash
    /// halfway through rewriting this file during a Recovery Key rotation would
    /// destroy the only recovery path at the exact moment the user was securing
    /// it. `rename` within a directory is atomic, so a reader sees the old file
    /// or the new one and never a half-written one.
    ///
    /// # Errors
    ///
    /// [`RecoveryError::Io`] if the directory cannot be created or written.
    pub fn write_to(&self, path: &Path) -> Result<(), RecoveryError> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .map_err(|e| RecoveryError::io("creating the data root", parent, e))?;
        }

        let temp = temp_path(path);
        write_owner_only(&temp, self.to_json().as_bytes())
            .map_err(|e| RecoveryError::io("writing the recovery file", &temp, e))?;
        std::fs::rename(&temp, path).map_err(|e| {
            // Do not leave the temp file behind to be mistaken for the real
            // one; the rename failing is already bad enough.
            let _ = std::fs::remove_file(&temp);
            RecoveryError::io("replacing the recovery file", path, e)
        })
    }

    /// Read from `path`.
    ///
    /// # Errors
    ///
    /// [`RecoveryError::NoBlob`] when the file is not there — its own variant,
    /// because "there is nothing to recover from" and "this is damaged" have
    /// different answers. [`RecoveryError::CorruptBlob`] otherwise.
    pub fn read_from(path: &Path) -> Result<Self, RecoveryError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(RecoveryError::NoBlob {
                    path: path.to_path_buf(),
                });
            }
            Err(e) => return Err(RecoveryError::io("reading the recovery file", path, e)),
        };
        Self::from_json(&text, path)
    }

    /// SHA-256 over everything public in the file, truncated to 8 bytes.
    ///
    /// Not a MAC. See the module docs for what this is and is not for.
    fn integrity(&self) -> [u8; INTEGRITY_LEN] {
        let mut h = Sha256::new();
        h.update(aad(self.kdf, &self.salt));
        h.update(self.nonce);
        h.update(self.sealed);
        let digest = h.finalize();
        let mut out = [0u8; INTEGRITY_LEN];
        out.copy_from_slice(&digest[..INTEGRITY_LEN]);
        out
    }
}

/// Argon2id over the raw Recovery Key bytes.
///
/// The **bytes**, not the displayed string: the display form carries dashes and
/// a prefix, and deriving from it would make the KEK depend on how the key was
/// formatted the day it was written.
fn derive_kek(
    recovery: &RecoveryKey,
    salt: &[u8; SALT_LEN],
    kdf: KdfParams,
) -> Result<[u8; 32], RecoveryError> {
    let params = argon2::Params::new(kdf.m_cost_kib, kdf.t_cost, kdf.p_cost, Some(32))
        .map_err(|e| RecoveryError::Crypto(format!("argon2 rejected the parameters: {e}")))?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);

    let mut kek = [0u8; 32];
    argon
        .hash_password_into(recovery.expose(), salt, &mut kek)
        .map_err(|e| RecoveryError::Crypto(format!("argon2 failed: {e}")))?;
    Ok(kek)
}

/// The associated data: version, parameters, salt.
///
/// Authenticating the parameters is what turns "somebody edited m_cost" from a
/// silent derivation of a different key into a tag failure.
fn aad(kdf: KdfParams, salt: &[u8; SALT_LEN]) -> Vec<u8> {
    let mut out = Vec::with_capacity(FORMAT_TAG.len() + 12 + SALT_LEN);
    out.extend_from_slice(FORMAT_TAG);
    out.extend_from_slice(&kdf.m_cost_kib.to_le_bytes());
    out.extend_from_slice(&kdf.t_cost.to_le_bytes());
    out.extend_from_slice(&kdf.p_cost.to_le_bytes());
    out.extend_from_slice(salt);
    out
}

/// N bytes from the OS CSPRNG.
///
/// Via `getrandom`, reached through the AEAD crate's `Generate` trait so this
/// tree carries no additional dependency for it. `getrandom` is the right layer
/// rather than a `/dev/urandom` read: on macOS it uses `getentropy(2)`, which
/// cannot fail on a file-descriptor exhaustion the way opening a device node
/// can, and it is the only one of the two that exists on Windows.
///
/// A failure means the OS has no entropy, which is not a condition to paper
/// over with a fallback — so it becomes an error and the caller aborts.
pub(super) fn os_random<const N: usize>() -> Result<[u8; N], RecoveryError> {
    <[u8; N]>::try_generate()
        .map_err(|e| RecoveryError::Crypto(format!("the OS CSPRNG failed: {e}")))
}

// -------------------------------------------------------------- tiny helpers

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// The raw text of `"name": <...>`, whether quoted or not.
fn raw_field<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let at = text.find(&format!("\"{name}\""))? + name.len() + 2;
    let rest = text[at..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    if let Some(quoted) = rest.strip_prefix('"') {
        return Some(&quoted[..quoted.find('"')?]);
    }
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    Some(&rest[..end]).filter(|s| !s.is_empty())
}

fn number(text: &str, name: &str) -> Option<u32> {
    raw_field(text, name)?.parse().ok()
}

fn fixed_hex<const N: usize>(text: &str, name: &str) -> Result<[u8; N], String> {
    let raw = raw_field(text, name).ok_or_else(|| format!("no {name} field"))?;
    if raw.len() != N * 2 {
        return Err(format!(
            "{name} is {} hex characters and must be {}; the file is truncated or padded",
            raw.len(),
            N * 2
        ));
    }
    let mut out = [0u8; N];
    for (i, chunk) in raw.as_bytes().chunks_exact(2).enumerate() {
        let s = std::str::from_utf8(chunk).map_err(|_| format!("{name} is not hex"))?;
        out[i] = u8::from_str_radix(s, 16).map_err(|_| format!("{name} is not hex"))?;
    }
    Ok(out)
}

/// `<path>.tmp-<pid>`, in the same directory so `rename` stays atomic.
fn temp_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".tmp-{}", std::process::id()));
    PathBuf::from(name)
}

/// Create with mode 0600 from the start, rather than chmod-ing afterwards —
/// which would leave a window in which the file is world-readable.
fn write_owner_only(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(bytes)?;
    // The point of this file is to survive the machine losing its keychain, so
    // it is also the file most likely to be read after an unclean shutdown.
    f.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cheap() -> KdfParams {
        KdfParams {
            m_cost_kib: 64,
            t_cost: 1,
            p_cost: 1,
        }
    }

    #[test]
    fn the_kek_depends_on_the_salt() {
        let rk = RecoveryKey::from_bytes([5; super::super::RECOVERY_KEY_BYTES]);
        let a = derive_kek(&rk, &[0; SALT_LEN], cheap()).unwrap();
        let b = derive_kek(&rk, &[1; SALT_LEN], cheap()).unwrap();
        assert_ne!(a, b, "the salt is not reaching argon2");
    }

    #[test]
    fn the_kek_depends_on_the_parameters() {
        let rk = RecoveryKey::from_bytes([5; super::super::RECOVERY_KEY_BYTES]);
        let a = derive_kek(&rk, &[0; SALT_LEN], cheap()).unwrap();
        let b = derive_kek(
            &rk,
            &[0; SALT_LEN],
            KdfParams {
                t_cost: 2,
                ..cheap()
            },
        )
        .unwrap();
        assert_ne!(a, b, "t_cost is not reaching argon2");
    }

    #[test]
    fn the_kek_is_not_the_recovery_key() {
        let rk = RecoveryKey::from_bytes([5; super::super::RECOVERY_KEY_BYTES]);
        let kek = derive_kek(&rk, &[0; SALT_LEN], cheap()).unwrap();
        assert_ne!(
            &kek[..super::super::RECOVERY_KEY_BYTES],
            rk.expose().as_slice(),
            "the KDF is the identity function"
        );
    }

    #[test]
    fn the_aad_covers_the_parameters_and_the_salt() {
        let a = aad(cheap(), &[0; SALT_LEN]);
        assert_ne!(a, aad(cheap(), &[1; SALT_LEN]));
        assert_ne!(
            a,
            aad(
                KdfParams {
                    m_cost_kib: 65,
                    ..cheap()
                },
                &[0; SALT_LEN]
            )
        );
        assert!(a.starts_with(FORMAT_TAG));
    }

    #[test]
    fn field_extraction_handles_numbers_and_strings() {
        let json = "{\n  \"a\": 17,\n  \"b\": \"cafe\",\n  \"c\": 0\n}";
        assert_eq!(number(json, "a"), Some(17));
        assert_eq!(raw_field(json, "b"), Some("cafe"));
        assert_eq!(number(json, "c"), Some(0));
        assert_eq!(number(json, "missing"), None);
    }

    #[test]
    fn fixed_hex_rejects_the_wrong_length_and_non_hex() {
        let json = "{\"salt\": \"00ff\"}";
        assert!(fixed_hex::<2>(json, "salt").is_ok());
        assert!(fixed_hex::<3>(json, "salt").is_err());
        assert!(fixed_hex::<2>("{\"salt\": \"zzzz\"}", "salt").is_err());
    }

    #[test]
    fn the_integrity_digest_changes_with_every_field() {
        let base = WrappedMasterKey {
            kdf: cheap(),
            salt: [0; SALT_LEN],
            nonce: [0; NONCE_LEN],
            sealed: [0; SEALED_LEN],
        };
        let d = base.integrity();

        let mut salt = base.clone();
        salt.salt[0] = 1;
        let mut nonce = base.clone();
        nonce.nonce[0] = 1;
        let mut sealed = base.clone();
        sealed.sealed[47] = 1;
        let mut kdf = base.clone();
        kdf.kdf.t_cost = 2;

        for other in [salt, nonce, sealed, kdf] {
            assert_ne!(d, other.integrity(), "a field is outside the digest");
        }
    }

    /// The ciphertext must not be in `Debug`. This type is logged as part of
    /// diagnostics and the sealed key is the one thing an offline attacker
    /// needs.
    #[test]
    fn debug_does_not_print_the_ciphertext() {
        let blob = WrappedMasterKey::wrap(
            &MasterKeyBytes::new([9; 32]),
            &RecoveryKey::from_bytes([8; super::super::RECOVERY_KEY_BYTES]),
            cheap(),
        )
        .unwrap();
        let rendered = format!("{blob:?}");
        assert!(!rendered.contains(&hex(&blob.sealed)), "{rendered}");
        assert!(rendered.contains("m_cost_kib"), "over-redacted: {rendered}");
    }
}
