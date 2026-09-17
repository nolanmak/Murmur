//! The one error type crossing the seam.

use std::io;

use crate::ids::{DeviceId, TapId};

/// Everything that can go wrong opening, starting, or running a tap.
///
/// Deliberately platform-neutral: an OS status code is carried as text in
/// [`TapError::Platform`] rather than as an `OSStatus`/`HRESULT` type, because
/// nothing above the seam may learn what OS it is running on.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TapError {
    /// This build cannot do what was asked — a platform without the
    /// capability, or a backend that is still a stub.
    ///
    /// Callers should have checked [`crate::PlatformCaps`] first; this is the
    /// backstop that keeps an unsupported request from silently degrading into
    /// a different capture than the user asked for.
    #[error("unsupported: {0}")]
    Unsupported(String),

    /// The user has not granted (or has revoked) the permission this tap
    /// needs.
    ///
    /// Note for macOS: the system-audio grant is *not* observable, and a
    /// denial delivers silence rather than an error (docs/REQUIREMENTS.md
    /// 6.3). Do not expect this variant to be the way a denied system tap
    /// announces itself there.
    #[error("permission denied: {0}")]
    PermissionDenied(String),

    /// No such device, or it disappeared between enumeration and open.
    #[error("no such device: {0}")]
    DeviceNotFound(DeviceId),

    /// The device exists but went away while it was in use. The layer above
    /// should rebuild rather than fail the recording.
    #[error("device invalidated: {0}")]
    DeviceInvalidated(TapId),

    /// The device cannot produce anything usable for this request.
    ///
    /// A *hint* that is not honoured is not this error — that is normal, and
    /// is reported by [`crate::AudioTap::format`] after `start()`.
    #[error("format not negotiable: {0}")]
    FormatUnsupported(String),

    /// `start()` on a tap that is already running.
    #[error("tap is already running")]
    AlreadyRunning,

    /// An operation that requires a running tap was called on a stopped one.
    #[error("tap is not running")]
    NotRunning,

    /// A caller-supplied value was invalid (a zero sample rate, a negative
    /// replay speed, ...).
    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    /// A fixture or stream could not be decoded.
    #[error("decode error: {0}")]
    Decode(String),

    /// I/O failure, with the operation that caused it.
    #[error("io error while {context}")]
    Io {
        /// What was being attempted, e.g. `reading fixtures/meeting.wav`.
        context: String,
        /// The underlying failure.
        #[source]
        source: io::Error,
    },

    /// The backend failed for a reason only it understands. The string carries
    /// the OS status; it is for logs and bug reports, never for control flow.
    #[error("platform error: {0}")]
    Platform(String),
}

impl TapError {
    /// Construct an [`TapError::Unsupported`] from anything stringy.
    pub fn unsupported(what: impl Into<String>) -> Self {
        Self::Unsupported(what.into())
    }

    /// Construct a [`TapError::Platform`] from anything stringy.
    pub fn platform(what: impl Into<String>) -> Self {
        Self::Platform(what.into())
    }

    /// Attach context to an [`io::Error`].
    pub fn io(context: impl Into<String>, source: io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }

    /// True for [`TapError::Unsupported`].
    ///
    /// Callers use this to fall back to a different scope (per-app capture ->
    /// full mix, say) instead of failing the recording.
    #[must_use]
    pub fn is_unsupported(&self) -> bool {
        matches!(self, Self::Unsupported(_))
    }

    /// True when the fix is a user action rather than a retry — the UI should
    /// route these to onboarding/recovery copy, not to a "try again" button.
    #[must_use]
    pub fn is_permission(&self) -> bool {
        matches!(self, Self::PermissionDenied(_))
    }

    /// True when the device went away underneath us and a rebuild is the
    /// correct response (CAP-06).
    #[must_use]
    pub fn is_transient_device_fault(&self) -> bool {
        matches!(self, Self::DeviceInvalidated(_) | Self::DeviceNotFound(_))
    }
}
