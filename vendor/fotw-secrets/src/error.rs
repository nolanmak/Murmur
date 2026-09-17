//! The one error type crossing the seam.

/// Everything that can go wrong storing, reading or validating a secret.
///
/// Deliberately platform-neutral: a keychain status is carried as text rather
/// than as a `keyring::Error`, for a reason specific to this crate. The
/// `keyring_core::Error::BadEncoding` variant holds the **raw credential
/// bytes**, and `Ambiguous` holds `Entry` values whose `Debug` reaches into
/// the platform store. Since error types derive `Debug` and errors get logged
/// with `{:?}` reflexively, embedding one would put key material one careless
/// log line away from disk. [`SecretsError::from_keyring`] flattens keyring
/// errors to vetted text at the boundary; nothing downstream can undo that.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SecretsError {
    /// No OS credential store is available (KEY-05).
    ///
    /// On Linux this is the no-Secret-Service case. It is a hard failure by
    /// design: there is no plaintext fallback to degrade to, and the payload
    /// is the platform's own explanation so the user is told what to install
    /// or unlock rather than just that "something went wrong".
    #[error("no OS secret service is available: {0}")]
    NoSecretService(String),

    /// The key has never been stored, or was deleted.
    #[error("no credential stored for {key}")]
    NotFound {
        /// The keyring account, e.g. `apikey:deepgram`. Never the material.
        key: String,
    },

    /// The store exists but refused us — typically a locked keychain, or a
    /// macOS ACL that does not list this binary.
    ///
    /// Distinct from [`SecretsError::NotFound`] because the fix is a user
    /// action, not a re-entry of the key.
    #[error("the OS credential store denied access to {key} while {operation}: {detail}")]
    AccessDenied {
        /// What was being attempted, e.g. `reading`.
        operation: &'static str,
        /// The keyring account.
        key: String,
        /// The platform's explanation.
        detail: String,
    },

    /// The credential store failed for a reason only it understands. The text
    /// is for logs and bug reports, never for control flow.
    #[error("the OS credential store failed while {operation} {key}: {detail}")]
    Platform {
        /// What was being attempted, e.g. `writing`.
        operation: &'static str,
        /// The keyring account.
        key: String,
        /// The platform's explanation.
        detail: String,
    },

    /// The supplied material cannot be a key — empty, or not something the
    /// provider could ever accept.
    #[error("invalid key material: {0}")]
    InvalidKeyMaterial(String),
}

impl SecretsError {
    /// Flatten a `keyring` error into a platform-neutral one, attaching the
    /// operation and the account it concerned.
    ///
    /// This is the **only** conversion from `keyring::Error`, and it is why
    /// there is no `From<keyring::Error>` impl: a blanket `?` conversion would
    /// let a future call site pull a `BadEncoding(Vec<u8>)` — the variant that
    /// carries raw credential bytes — into a `Debug`-derived error type
    /// without noticing. Requiring the operation and key at the call site
    /// makes the conversion a decision rather than a coercion.
    pub fn from_keyring(operation: &'static str, key: &str, err: keyring::Error) -> Self {
        match err {
            keyring::Error::NoEntry => Self::NotFound {
                key: key.to_owned(),
            },
            // The store never initialised. On Linux that is the Secret
            // Service being absent, which is exactly KEY-05.
            keyring::Error::NoDefaultStore => Self::NoSecretService(
                "the platform credential store could not be initialised".to_owned(),
            ),
            keyring::Error::NoStorageAccess(detail) => Self::AccessDenied {
                operation,
                key: key.to_owned(),
                detail: detail.to_string(),
            },
            // `Ambiguous`'s own `Display` renders the matching `Entry` values
            // with `{:?}`, reaching into platform credential state. Substitute
            // a fixed message rather than forward it: the operator needs to
            // know the keychain has duplicate entries for this account, and
            // needs nothing else from the payload.
            keyring::Error::Ambiguous(_) => Self::Platform {
                operation,
                key: key.to_owned(),
                detail: "several credentials in the keychain match this account; \
                         remove the duplicates and re-enter the key"
                    .to_owned(),
            },
            // Every remaining variant has a `Display` that has been checked
            // not to render its payload buffers (`BadEncoding` prints only
            // "Password data is not valid UTF-8"; `BadDataFormat` prints only
            // its platform error).
            other => Self::Platform {
                operation,
                key: key.to_owned(),
                detail: other.to_string(),
            },
        }
    }

    /// True when the secret simply is not stored.
    ///
    /// Callers use this to route to onboarding ("add your Deepgram key")
    /// rather than to an error toast.
    #[must_use]
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound { .. })
    }

    /// True for the KEY-05 hard failure.
    ///
    /// The UI must treat this as blocking: there is no key storage on this
    /// machine, so there is nowhere safe to put what the user is about to
    /// type.
    #[must_use]
    pub fn is_no_secret_service(&self) -> bool {
        matches!(self, Self::NoSecretService(_))
    }
}

#[cfg(test)]
mod tests {
    use crate::SecretsError;

    /// The subtle one. `keyring_core::Error::BadEncoding(Vec<u8>)` carries the
    /// *raw credential bytes* — its `Display` says only "Password data is not
    /// valid UTF-8", but its `Debug` prints the buffer. Since `SecretsError`
    /// derives `Debug` and errors get logged with `{:?}` as a matter of
    /// course, wrapping a keyring error by anything other than its `Display`
    /// string would pipe key material straight into the log.
    ///
    /// [`SecretsError::from_keyring`] is the only conversion, and it renders
    /// through `Display`. This test pins that.
    #[test]
    fn keyring_errors_are_flattened_to_their_display_text() {
        let leaky = keyring::Error::BadEncoding(b"sk-live-RAW-SECRET-BYTES".to_vec());

        // Prove the hazard is real before proving we avoid it.
        assert!(
            format!("{leaky:?}").contains("115"), // b's = 115, i.e. the bytes are in there
            "keyring's Debug no longer prints the raw buffer; revisit this guard"
        );

        let ours = SecretsError::from_keyring("reading", "apikey:openai", leaky);
        let rendered = format!("{ours:?}");
        assert!(
            !rendered.contains("sk-live-RAW-SECRET-BYTES"),
            "wrapped error leaked key material: {rendered}"
        );
        assert!(rendered.contains("apikey:openai"), "no context: {rendered}");
    }

    #[test]
    fn missing_entries_become_not_found() {
        let err = SecretsError::from_keyring("reading", "db:masterkey", keyring::Error::NoEntry);
        assert!(err.is_not_found());
        assert!(!err.is_no_secret_service());
    }

    /// A locked keychain is a *user action* ("unlock your keychain"), not a
    /// missing key and not a bug. Callers route these to recovery copy, so
    /// they must be distinguishable without string-matching the message.
    #[test]
    fn an_uninitialised_store_is_reported_as_no_secret_service() {
        let err = SecretsError::from_keyring(
            "writing",
            "apikey:deepgram",
            keyring::Error::NoDefaultStore,
        );
        assert!(err.is_no_secret_service());
        assert!(!err.is_not_found());
    }

    #[test]
    fn display_never_mentions_material_only_the_account() {
        let err = SecretsError::NotFound {
            key: "apikey:anthropic".to_owned(),
        };
        assert_eq!(err.to_string(), "no credential stored for apikey:anthropic");
    }
}
