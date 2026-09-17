//! The credentials index — the only key-related row the database may hold.

use serde::{Deserialize, Serialize};

use crate::{Fingerprint, SecretKey, SecretString};

/// One row of the credentials index (docs/REQUIREMENTS.md 10).
///
/// > The DB holds only a credentials index — `(id, provider, keyring_service,
/// > keyring_account, fingerprint, label, created_at, last_used_at)` where
/// > `fingerprint` is the first 16 hex chars of SHA-256 so the UI can say
/// > which key is configured.
///
/// The shape *is* the control. Every field is either an identifier, a
/// timestamp, or a digest; there is no field a key could be put in, and
/// [`CredentialRecord::describe`] is the only constructor — it takes a
/// [`SecretString`] and keeps only its fingerprint. A future field that could
/// carry material would have to be added here deliberately, and would fail the
/// column test next door.
///
/// # What this row is *for*
///
/// It answers "is a Deepgram key configured, and is it the same one as last
/// week?" without the database being able to answer "what is it?". That is
/// what makes the encrypted-at-rest story honest: even with the SQLCipher key,
/// the credentials table yields nothing usable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CredentialRecord {
    /// Row identity. A ULID/UUIDv7 minted by `fotw-store`, which owns id
    /// generation for every table (see that crate's `ids` module) — passing it
    /// in rather than generating it here keeps this crate free of a
    /// randomness dependency and keeps one id scheme in one place.
    pub id: String,
    /// Provider slug, or `db` for the master key.
    pub provider: String,
    /// The keychain service the material actually lives under.
    pub keyring_service: String,
    /// The keychain account, e.g. `apikey:deepgram`.
    pub keyring_account: String,
    /// First 16 hex chars of SHA-256 of the material.
    pub fingerprint: Fingerprint,
    /// Optional user-supplied name, e.g. "work account", for people who
    /// rotate between two keys.
    pub label: Option<String>,
    /// Unix milliseconds, matching every other timestamp column in the schema.
    pub created_at: i64,
    /// Unix milliseconds of the last successful use, or `None` if never used.
    pub last_used_at: Option<i64>,
}

impl CredentialRecord {
    /// The column names, in schema order.
    ///
    /// Exported so `fotw-store` can build its `INSERT` from one source of
    /// truth, and so the test next door can assert the serialised shape has
    /// not grown a ninth field.
    pub const COLUMNS: &'static [&'static str] = &[
        "id",
        "provider",
        "keyring_service",
        "keyring_account",
        "fingerprint",
        "label",
        "created_at",
        "last_used_at",
    ];

    /// Describe a stored secret, without retaining it.
    ///
    /// The secret is borrowed, fingerprinted, and dropped by the caller. This
    /// is the only way to build a record, which is what makes "the DB cannot
    /// hold a key" a property of the code rather than of everyone's memory.
    #[must_use]
    pub fn describe(
        id: impl Into<String>,
        key: SecretKey,
        secret: &SecretString,
        label: Option<String>,
        created_at: i64,
    ) -> Self {
        Self {
            id: id.into(),
            provider: key.provider_slug().to_owned(),
            keyring_service: key.service().to_owned(),
            keyring_account: key.account(),
            fingerprint: Fingerprint::of(secret),
            label,
            created_at,
            last_used_at: None,
        }
    }

    /// The key this row points at, or `None` if the row was written by a
    /// version that knew about a provider this one does not.
    #[must_use]
    pub fn key(&self) -> Option<SecretKey> {
        SecretKey::from_account(&self.keyring_account)
    }

    /// Record a successful use at `now_ms`.
    pub fn touch(&mut self, now_ms: i64) {
        self.last_used_at = Some(now_ms);
    }

    /// Whether this row describes the given secret.
    ///
    /// The point of the fingerprint: confirms the keychain still holds the key
    /// the row was written for, without either side revealing the material.
    #[must_use]
    pub fn matches(&self, secret: &SecretString) -> bool {
        self.fingerprint == Fingerprint::of(secret)
    }
}

#[cfg(test)]
mod tests {
    use crate::{CredentialRecord, Fingerprint, Provider, SecretKey, SecretString};

    const KEY_MATERIAL: &str = "sk-live-51H8vQeaCOMPLETELYSECRETVALUE";

    fn record() -> CredentialRecord {
        CredentialRecord::describe(
            "01JD8QK0000000000000000000",
            SecretKey::ApiKey(Provider::OpenAi),
            &SecretString::new(KEY_MATERIAL),
            Some("work account".to_owned()),
            1_754_000_000_000,
        )
    }

    /// docs/REQUIREMENTS.md 10 names the eight columns exactly. This asserts
    /// the set, so adding a ninth that could carry key material fails here
    /// rather than in a security review.
    #[test]
    fn holds_exactly_the_eight_columns_the_spec_allows() {
        let json = serde_json::to_value(record()).unwrap();
        let obj = json.as_object().unwrap();

        let mut keys: Vec<_> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "created_at",
                "fingerprint",
                "id",
                "keyring_account",
                "keyring_service",
                "label",
                "last_used_at",
                "provider",
            ]
        );

        let mut columns = CredentialRecord::COLUMNS.to_vec();
        columns.sort_unstable();
        assert_eq!(keys, columns, "COLUMNS drifted from the struct");
    }

    /// KEY-01 at the type level: the material goes in, and only the
    /// fingerprint comes out. There is no constructor that stores the key.
    #[test]
    fn serialised_record_carries_the_fingerprint_and_never_the_key() {
        let record = record();
        let json = serde_json::to_string(&record).unwrap();

        assert!(
            !json.contains(KEY_MATERIAL),
            "record leaked the key: {json}"
        );
        assert!(
            !json.contains("51H8vQea"),
            "record leaked a key prefix: {json}"
        );
        assert!(json.contains(record.fingerprint.as_str()));
        assert!(json.contains("apikey:openai"));
        assert!(json.contains("work account"));

        // The Debug rendering is the other way this reaches a log.
        let debug = format!("{record:?}");
        assert!(!debug.contains(KEY_MATERIAL), "Debug leaked: {debug}");
    }

    #[test]
    fn fingerprint_identifies_the_key_it_was_built_from() {
        let secret = SecretString::new(KEY_MATERIAL);
        assert_eq!(record().fingerprint, Fingerprint::of(&secret));
    }

    #[test]
    fn derives_keyring_coordinates_from_the_key() {
        let record = record();
        assert_eq!(record.keyring_service, crate::KEYRING_SERVICE);
        assert_eq!(record.keyring_account, "apikey:openai");
        assert_eq!(record.provider, "openai");
        assert_eq!(record.created_at, 1_754_000_000_000);
        assert_eq!(
            record.last_used_at, None,
            "a new credential has not been used"
        );
    }

    #[test]
    fn master_key_records_use_the_db_provider_slug() {
        let record = CredentialRecord::describe(
            "01JD8QK0000000000000000001",
            SecretKey::DbMasterKey,
            &SecretString::new("32-bytes-of-csprng-output-here!!"),
            None,
            1,
        );
        assert_eq!(record.provider, "db");
        assert_eq!(record.keyring_account, "db:masterkey");
    }

    #[test]
    fn touch_records_last_use() {
        let mut record = record();
        record.touch(1_754_000_009_999);
        assert_eq!(record.last_used_at, Some(1_754_000_009_999));
    }

    #[test]
    fn round_trips_through_serde() {
        let record = record();
        let json = serde_json::to_string(&record).unwrap();
        let back: CredentialRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(record, back);
    }
}
