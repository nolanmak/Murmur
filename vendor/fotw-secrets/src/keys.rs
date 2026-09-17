//! Which secrets exist, and what they are called in the OS keychain.

use std::fmt;

/// The keychain *service* every FlyOnTheWall credential lives under.
///
/// One service, one entry per account. A reverse-DNS identifier because that
/// is what macOS Keychain Access shows the user in its list — "fotw" would be
/// unattributable next to two hundred other entries.
pub const KEYRING_SERVICE: &str = "com.flyonthewall.fotw";

/// A cloud provider we hold an API key for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Provider {
    /// Deepgram — default cloud STT (docs/REQUIREMENTS.md 7.1).
    Deepgram,
    /// ElevenLabs — long-form / re-transcription STT.
    ElevenLabs,
    /// OpenAI — STT and summarization.
    OpenAi,
    /// Anthropic — summarization.
    Anthropic,
}

impl Provider {
    /// Every provider, in the order the settings screen lists them.
    pub const ALL: [Self; 4] = [
        Self::Deepgram,
        Self::ElevenLabs,
        Self::OpenAi,
        Self::Anthropic,
    ];

    /// The lowercase identifier used in keychain accounts and in the
    /// `provider` column of the credentials index.
    ///
    /// This is persisted data. Renaming a slug orphans a user's key.
    #[must_use]
    pub fn slug(self) -> &'static str {
        match self {
            Self::Deepgram => "deepgram",
            Self::ElevenLabs => "elevenlabs",
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
        }
    }

    /// Parse a slug back into a provider. `None` for anything unrecognised —
    /// a row written by a newer version must not be guessed at.
    #[must_use]
    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.slug() == slug)
    }

    /// The provider's name as the user knows it, for UI and for the
    /// third-party disclosure screen (KEY-04).
    #[must_use]
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Deepgram => "Deepgram",
            Self::ElevenLabs => "ElevenLabs",
            Self::OpenAi => "OpenAI",
            Self::Anthropic => "Anthropic",
        }
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.slug())
    }
}

/// Which secret, of the fixed set this app stores.
///
/// An enum rather than a free-form string so that "what does FlyOnTheWall keep
/// in your keychain?" has an answer that is checked by the compiler.
/// docs/REQUIREMENTS.md 10 also anticipates `plugin:<id>:<key>` entries; those
/// are deliberately absent until the plugin interface (EXP-08) exists, because
/// an open-ended variant here would defeat the enumeration that
/// [`SecretKey::ALL`] gives the acceptance test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum SecretKey {
    /// A provider API key, stored as `apikey:<slug>`.
    ApiKey(Provider),
    /// The 32-byte database master key, stored as `db:masterkey`
    /// (docs/REQUIREMENTS.md 10, "Encryption at rest").
    DbMasterKey,
}

impl SecretKey {
    /// Every secret this application will ever put in the keychain.
    ///
    /// The acceptance test walks this to guarantee it covers all of them: a
    /// new variant that nobody adds to the scan would make KEY-01's test
    /// quietly narrower, so the test iterates this rather than a hand-written
    /// list.
    pub const ALL: [Self; 5] = [
        Self::ApiKey(Provider::Deepgram),
        Self::ApiKey(Provider::ElevenLabs),
        Self::ApiKey(Provider::OpenAi),
        Self::ApiKey(Provider::Anthropic),
        Self::DbMasterKey,
    ];

    /// The keychain *account* for this secret, e.g. `apikey:deepgram`.
    ///
    /// Persisted in the credentials index and in the OS keychain itself.
    #[must_use]
    pub fn account(self) -> String {
        match self {
            Self::ApiKey(provider) => format!("apikey:{}", provider.slug()),
            Self::DbMasterKey => "db:masterkey".to_owned(),
        }
    }

    /// The keychain *service*. Constant today; a method so that a future
    /// per-profile or per-install namespace does not have to change callers.
    #[must_use]
    pub fn service(self) -> &'static str {
        KEYRING_SERVICE
    }

    /// Parse an account name back into a key. `None` for anything this
    /// version does not know — including a well-formed `plugin:` account.
    #[must_use]
    pub fn from_account(account: &str) -> Option<Self> {
        if account == "db:masterkey" {
            return Some(Self::DbMasterKey);
        }
        let slug = account.strip_prefix("apikey:")?;
        Provider::from_slug(slug).map(Self::ApiKey)
    }

    /// The provider this key belongs to, or `None` for the master key.
    #[must_use]
    pub fn provider(self) -> Option<Provider> {
        match self {
            Self::ApiKey(provider) => Some(provider),
            Self::DbMasterKey => None,
        }
    }

    /// The value for the credentials index's `provider` column.
    ///
    /// `db` for the master key, so the column is never null and the UI can
    /// group rows without a special case.
    #[must_use]
    pub fn provider_slug(self) -> &'static str {
        match self {
            Self::ApiKey(provider) => provider.slug(),
            Self::DbMasterKey => "db",
        }
    }
}

impl fmt::Display for SecretKey {
    /// The account name. Safe to log — it names a secret without being one.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.account())
    }
}

#[cfg(test)]
mod tests {
    use crate::{KEYRING_SERVICE, Provider, SecretKey};

    /// The account names are a wire format: they are written into the
    /// `keyring_account` column (docs/REQUIREMENTS.md 10) and into the user's
    /// actual keychain, so changing one orphans every existing credential.
    /// This test exists to make that breakage loud.
    #[test]
    fn account_names_match_the_spec() {
        assert_eq!(
            SecretKey::ApiKey(Provider::Deepgram).account(),
            "apikey:deepgram"
        );
        assert_eq!(
            SecretKey::ApiKey(Provider::ElevenLabs).account(),
            "apikey:elevenlabs"
        );
        assert_eq!(
            SecretKey::ApiKey(Provider::OpenAi).account(),
            "apikey:openai"
        );
        assert_eq!(
            SecretKey::ApiKey(Provider::Anthropic).account(),
            "apikey:anthropic"
        );
        assert_eq!(SecretKey::DbMasterKey.account(), "db:masterkey");
    }

    #[test]
    fn every_key_shares_one_service_and_a_unique_account() {
        let mut seen = std::collections::BTreeSet::new();
        for key in SecretKey::ALL {
            assert_eq!(key.service(), KEYRING_SERVICE);
            assert!(seen.insert(key.account()), "duplicate account for {key:?}");
        }
        assert_eq!(
            seen.len(),
            5,
            "expected one entry per provider plus the master key"
        );
    }

    #[test]
    fn accounts_round_trip() {
        for key in SecretKey::ALL {
            assert_eq!(SecretKey::from_account(&key.account()), Some(key));
        }
        assert_eq!(SecretKey::from_account("apikey:nonesuch"), None);
        assert_eq!(SecretKey::from_account("plugin:foo:bar"), None);
        assert_eq!(SecretKey::from_account(""), None);
    }

    #[test]
    fn provider_slugs_round_trip() {
        for provider in Provider::ALL {
            assert_eq!(Provider::from_slug(provider.slug()), Some(provider));
            assert!(!provider.display_name().is_empty());
        }
        assert_eq!(Provider::from_slug("Deepgram"), None, "slugs are lowercase");
    }

    /// A `SecretKey` names a secret; it never holds one. Printing it is safe
    /// and is what a log line needs in place of the key itself.
    #[test]
    fn keys_are_printable() {
        assert_eq!(
            format!("{}", SecretKey::ApiKey(Provider::OpenAi)),
            "apikey:openai"
        );
        assert_eq!(
            SecretKey::ApiKey(Provider::Anthropic).provider(),
            Some(Provider::Anthropic)
        );
        assert_eq!(SecretKey::DbMasterKey.provider(), None);
    }
}
