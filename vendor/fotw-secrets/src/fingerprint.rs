//! The one non-secret thing the database is allowed to know about a key.

use std::fmt::{self, Write as _};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::SecretString;

/// The first 16 hex characters of the SHA-256 of a secret.
///
/// This is what lets the settings screen say "Deepgram: key ending
/// …configured" without the database ever holding a key
/// (docs/REQUIREMENTS.md 10). It is deliberately *not* a truncation of the key
/// itself — a last-4-characters display, the obvious alternative, hands an
/// attacker with database access four known characters of every key.
///
/// # Why 64 bits is enough, and why it is not too much
///
/// 16 hex chars is 64 bits of a preimage-resistant hash. Enough that two of a
/// user's keys will not collide (they have at most five), and enough to
/// confirm "the key in the keychain is the one this row describes". Not enough
/// to brute-force a key back out: even for a provider with a known key format,
/// the search space of an API key is far beyond what 64 bits of digest
/// narrows. Publishing the *full* digest would be the mistake — it would let
/// anyone holding a candidate key confirm it offline against a stolen
/// database.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Fingerprint(String);

impl Fingerprint {
    /// Number of hex characters in a fingerprint.
    pub const LEN: usize = 16;

    /// Fingerprint a secret.
    #[must_use]
    pub fn of(secret: &SecretString) -> Self {
        Self::of_bytes(secret.expose().as_bytes())
    }

    /// Fingerprint raw bytes.
    ///
    /// Private on purpose: taking a `&SecretString` at the public boundary is
    /// what keeps a caller from fingerprinting a key they are holding as a
    /// bare `String` — if they can call this, they already have the problem
    /// this crate exists to prevent.
    fn of_bytes(bytes: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let digest = hasher.finalize();

        let mut hex = String::with_capacity(Self::LEN);
        for byte in &digest[..Self::LEN / 2] {
            // Infallible: writing to a String.
            let _ = write!(hex, "{byte:02x}");
        }
        Self(hex)
    }

    /// The fingerprint as lowercase hex.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for Fingerprint {
    /// Shows the fingerprint. Unlike [`crate::SecretString`], this type is
    /// *meant* to be printed — redacting it would make the UI useless and
    /// teach callers that redaction is noise to be worked around.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({})", self.0)
    }
}

#[cfg(test)]
mod tests {
    use crate::{Fingerprint, SecretString};

    /// Interop matters more than it looks: a fingerprint we cannot reproduce
    /// with `shasum -a 256 | cut -c1-16` is a fingerprint we cannot verify by
    /// hand when a support ticket says "the UI shows the wrong key".
    #[test]
    fn matches_the_standard_sha256_of_the_material() {
        // printf '' | shasum -a 256 -> e3b0c44298fc1c149afbf4c8996fb924...
        assert_eq!(
            Fingerprint::of(&SecretString::new("")).as_str(),
            "e3b0c44298fc1c14"
        );
        // printf 'abc' | shasum -a 256 -> ba7816bf8f01cfea414140de5dae2223...
        assert_eq!(
            Fingerprint::of(&SecretString::new("abc")).as_str(),
            "ba7816bf8f01cfea"
        );
    }

    #[test]
    fn is_sixteen_lowercase_hex_chars() {
        let fp = Fingerprint::of(&SecretString::new("dg-test-key-deepgram-000000000000"));
        assert_eq!(fp.as_str().len(), Fingerprint::LEN);
        assert_eq!(fp.as_str().len(), 16);
        assert!(
            fp.as_str()
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "not lowercase hex: {fp}"
        );
        assert_eq!(fp.as_str(), "493b37c310a277b8");
    }

    #[test]
    fn is_deterministic_and_distinguishes_keys() {
        let a = Fingerprint::of(&SecretString::new("sk-aaaa"));
        let again = Fingerprint::of(&SecretString::new("sk-aaaa"));
        let b = Fingerprint::of(&SecretString::new("sk-bbbb"));

        assert_eq!(a, again, "not deterministic");
        assert_ne!(a, b, "collided on trivially different keys");
    }

    /// The reason a fingerprint may live in the database at all: it is not the
    /// key, and no prefix of the key survives into it.
    #[test]
    fn does_not_contain_the_material() {
        let material = "sk-live-51H8vQeatCOMPLETESECRET";
        let fp = Fingerprint::of(&SecretString::new(material));

        assert!(!fp.as_str().contains(material));
        assert!(!format!("{fp:?}").contains(material));
        // Every 4-char window of the key, in case the fingerprint were ever
        // "reduced" to a prefix by a well-meaning refactor.
        for window in material.as_bytes().windows(4) {
            let needle = std::str::from_utf8(window).unwrap();
            assert!(!fp.as_str().contains(needle), "fingerprint leaked {needle}");
        }
    }

    /// Unlike [`crate::SecretString`], a fingerprint is *meant* to be printed
    /// — that is its entire job. Redacting it here would make the UI useless
    /// and teach callers that redaction is noise to work around.
    #[test]
    fn display_and_debug_show_the_fingerprint() {
        let fp = Fingerprint::of(&SecretString::new("abc"));
        assert_eq!(format!("{fp}"), "ba7816bf8f01cfea");
        assert!(format!("{fp:?}").contains("ba7816bf8f01cfea"));
    }

    #[test]
    fn round_trips_through_serde_as_a_plain_string() {
        let fp = Fingerprint::of(&SecretString::new("abc"));
        let json = serde_json::to_string(&fp).unwrap();
        assert_eq!(json, "\"ba7816bf8f01cfea\"");
        let back: Fingerprint = serde_json::from_str(&json).unwrap();
        assert_eq!(fp, back);
    }
}
