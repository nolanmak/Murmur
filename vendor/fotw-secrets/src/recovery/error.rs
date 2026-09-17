//! Why a recovery attempt failed — and above all, *which* failure it was.
//!
//! # The whole reason this enum is not one variant
//!
//! SQLCipher reports a wrong `PRAGMA key` as **"file is not a database"**. A
//! user who typed their Recovery Key slightly wrong and got that message would
//! conclude their library is corrupt and start hunting a data-corruption bug
//! that does not exist — possibly restoring over a perfectly good file while
//! they are at it. So every failure on the recovery path has to be attributable
//! *before* SQLCipher is ever handed a key, and the variants below are the four
//! distinct things a user can actually do about it:
//!
//! | variant | what happened | what the user does |
//! |---|---|---|
//! | [`Malformed`](RecoveryError::Malformed) | the checksum rejected what was typed | re-read the card; a character is wrong |
//! | [`WrongRecoveryKey`](RecoveryError::WrongRecoveryKey) | a valid key, but not this library's | find the other card |
//! | [`CorruptBlob`](RecoveryError::CorruptBlob) | the sealed file is damaged | restore the file from a backup |
//! | [`NoBlob`](RecoveryError::NoBlob) | there is no sealed file here | check the folder; the key alone is not enough |
//!
//! Nothing in this list is ever "database is corrupt", because in none of these
//! cases is it.

use std::path::PathBuf;

/// Everything that can go wrong on the Recovery Key path.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RecoveryError {
    /// What was typed is not a well-formed Recovery Key.
    ///
    /// A transcription error: wrong length, a character outside the alphabet,
    /// or a failed checksum. **Nothing was tried against the library** — this
    /// is caught before any key derivation runs, so it costs nothing and rules
    /// out the far more alarming diagnosis.
    #[error(
        "that does not look like a Recovery Key: {0}\n  \
         Recovery Keys look like fotw1-xxxx-xxxx-xxxx-xxxx-xxxx-xxxx-xxxx-xxxx. \
         Nothing was tried against your library — check what you typed."
    )]
    Malformed(String),

    /// A well-formed Recovery Key that does not open this library.
    ///
    /// The sealed key failed to authenticate. That means one of two things and
    /// the message says both, because we cannot tell them apart: an AEAD tag
    /// mismatch is a tag mismatch whether the key was wrong or the ciphertext
    /// was edited. What it categorically does **not** mean is that the database
    /// is damaged; the database has not been opened at this point.
    #[error(
        "that Recovery Key does not open this library.\n  \
         The key is well-formed — the checksum passed — but the sealed master \
         key did not authenticate under it. Either it is the Recovery Key for a \
         different library, or {path} has been modified.\n  \
         Your database is NOT corrupt: nothing has opened it, and the key in \
         your keychain (if you still have it) still works."
    )]
    WrongRecoveryKey {
        /// The sealed-blob file the attempt was made against.
        path: PathBuf,
    },

    /// The sealed blob is damaged or was written by something else.
    ///
    /// Distinguished from [`WrongRecoveryKey`](RecoveryError::WrongRecoveryKey)
    /// by a key-independent integrity digest over the file's own fields, so
    /// "you mistyped" and "this file is damaged" are never confused for one
    /// another. See [`super::blob`] for what that digest can and cannot detect.
    #[error("the recovery file at {path} is damaged: {detail}")]
    CorruptBlob {
        /// The file that could not be read.
        path: PathBuf,
        /// What was wrong with it.
        detail: String,
    },

    /// There is no sealed blob beside this library.
    ///
    /// Either the library predates issue #38, or the file was deleted or lost
    /// in a partial restore. A Recovery Key on its own cannot open anything:
    /// it unwraps the sealed master key, and there is nothing here to unwrap.
    #[error(
        "there is no recovery file at {path}.\n  \
         A Recovery Key unwraps the sealed master key stored in that file; on \
         its own it cannot open a library. Restore it from the same backup as \
         db.sqlite3."
    )]
    NoBlob {
        /// Where the file was expected.
        path: PathBuf,
    },

    /// A cryptographic primitive or the OS CSPRNG failed.
    ///
    /// Not something a user can act on, and deliberately not merged into the
    /// variants above: an Argon2 parameter rejection is a bug in this program,
    /// not a mistyped key, and reporting it as one would send the user chasing
    /// a transcription error forever.
    #[error("recovery key cryptography failed: {0}")]
    Crypto(String),

    /// Reading or writing the sealed blob failed.
    #[error("io error while {context} ({path})")]
    Io {
        /// What was being attempted.
        context: &'static str,
        /// The path involved.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
}

impl RecoveryError {
    /// Attach a path and an operation to an [`std::io::Error`].
    pub(crate) fn io(
        context: &'static str,
        path: impl Into<PathBuf>,
        source: std::io::Error,
    ) -> Self {
        Self::Io {
            context,
            path: path.into(),
            source,
        }
    }

    /// True when the user mistyped: the fix is to look at the card again.
    #[must_use]
    pub fn is_malformed(&self) -> bool {
        matches!(self, Self::Malformed(_))
    }

    /// True when the key was well-formed but is not this library's.
    #[must_use]
    pub fn is_wrong_key(&self) -> bool {
        matches!(self, Self::WrongRecoveryKey { .. })
    }

    /// True when the sealed file itself is the problem.
    #[must_use]
    pub fn is_corrupt_blob(&self) -> bool {
        matches!(self, Self::CorruptBlob { .. })
    }

    /// True when there is no sealed file to recover from.
    #[must_use]
    pub fn is_missing_blob(&self) -> bool {
        matches!(self, Self::NoBlob { .. })
    }
}
