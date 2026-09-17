//! The seam itself: [`AudioTap`] and [`AudioPlatform`].
//!
//! Nothing above this crate may name an operating-system type. See
//! docs/REQUIREMENTS.md 6.5 for the five seam rules these traits encode.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use crate::device_change::DeviceChangeSignal;
use crate::error::TapError;
use crate::events::PlatformEvent;
use crate::format::{FormatRequest, StreamFormat};
use crate::frames::FrameSink;
use crate::ids::{AppInfo, AppRef, DeviceId, DeviceInfo, TapId};
use crate::permission::{Permission, PermissionState, PlatformCaps};
use crate::watchdog::{OutputActivity, OutputProbe};

/// A boxed future, so [`AudioPlatform`] stays object-safe without pulling an
/// async runtime into the seam.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What slice of system audio to capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemScope {
    /// Everything going to the default output.
    DefaultOutputMix,
    /// Only these applications.
    Apps(Vec<AppRef>),
    /// Everything except these applications — how we keep our own playback out
    /// of the recording.
    AllExcept(Vec<AppRef>),
}

/// One capture stream.
///
/// Seam rule 2: a tap is always one *logical track*. The mic leg and the
/// system leg are separate taps and are never pre-mixed, because no platform's
/// loopback separates individual remote speakers — keeping them apart is what
/// makes "me vs them" free everywhere.
pub trait AudioTap: Send {
    /// Stable identity: `mic:<uid>` | `system:default` | `system:app:<key>`.
    fn id(&self) -> &TapId;

    /// The stream format.
    ///
    /// **Seam rule 1.** Before [`AudioTap::start`] this is only an echo of the
    /// requested hint and [`AudioTap::format_is_authoritative`] returns false.
    /// After a successful start it is what the platform actually gave us,
    /// which is frequently not what was asked for: Windows process loopback is
    /// fixed at 44.1 kHz / 2 ch / S16 because `GetMixFormat` returns
    /// `E_NOTIMPL` on that client.
    fn format(&self) -> StreamFormat;

    /// Whether [`AudioTap::format`] currently reports observed truth.
    fn format_is_authoritative(&self) -> bool;

    /// Begin delivering frames to `sink`, returning the authoritative format.
    fn start(&mut self, sink: Box<dyn FrameSink>) -> Result<StreamFormat, TapError>;

    /// Stop delivering frames. Idempotent.
    fn stop(&mut self) -> Result<(), TapError>;
}

impl fmt::Debug for dyn AudioTap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AudioTap")
            .field("id", &self.id())
            .field("format", &self.format())
            .field("authoritative", &self.format_is_authoritative())
            .finish()
    }
}

/// A platform's audio backend.
pub trait AudioPlatform: Send + Sync {
    /// What this backend can do. Callers branch on this, never on the OS name.
    fn caps(&self) -> PlatformCaps;

    /// Current state of `permission`.
    fn permission(&self, permission: Permission) -> PermissionState;

    /// Ask the user for `permission`.
    ///
    /// Must resolve rather than hang on platforms where the permission does
    /// not exist — returning [`PermissionState::NotApplicable`] verbatim.
    fn request_permission(&self, permission: Permission) -> BoxFuture<'static, PermissionState>;

    /// Available microphones.
    fn mics(&self) -> Vec<DeviceInfo>;

    /// Applications currently producing audio. Empty where unsupported.
    fn capturable_apps(&self) -> Vec<AppInfo>;

    /// Open a microphone tap.
    fn open_mic(
        &self,
        device: &DeviceId,
        hint: FormatRequest,
    ) -> Result<Box<dyn AudioTap>, TapError>;

    /// Open a system-audio tap.
    ///
    /// Asking for a scope the backend does not support is a typed
    /// [`TapError::Unsupported`], never a silent downgrade to full-system
    /// capture — recording more than the user asked for is a privacy failure,
    /// not a fallback.
    fn open_system(
        &self,
        scope: SystemScope,
        hint: FormatRequest,
    ) -> Result<Box<dyn AudioTap>, TapError>;

    /// Subscribe to device-change notifications.
    fn events(&self) -> Receiver<PlatformEvent>;

    /// Install the platform's device-change notifications, raising into
    /// `signal`.
    ///
    /// The real-time-safe sibling of [`AudioPlatform::events`]. macOS delivers
    /// these on a Core Audio-owned thread that may not block or allocate, and
    /// publishing onto an `mpsc` channel does both — so the notification path
    /// is a lock-free bitmask and the fan-out happens on the caller's thread.
    ///
    /// The returned guard **must be held** for the length of the session:
    /// dropping it unregisters the listeners with no diagnostic anywhere.
    ///
    /// The default installs nothing and succeeds, because a platform with no
    /// device notifications is a limitation and not an error — the stall
    /// watchdog is still the backstop there.
    fn watch_devices(
        &self,
        signal: Arc<DeviceChangeSignal>,
    ) -> Result<Box<dyn DeviceWatch>, TapError> {
        let _ = signal;
        Ok(Box::new(()))
    }

    /// Whether anything on this machine is currently rendering output audio.
    ///
    /// The corroborating evidence for CAP-05's silence rule: bit-exact zero
    /// buffers mean nothing on their own, because a quiet meeting produces
    /// exactly that, and only "something *is* playing and we are still getting
    /// zeros" is a fault (docs/REQUIREMENTS.md 6.4).
    ///
    /// The default is [`OutputActivity::Unknown`], which **disables** the
    /// silence rule rather than deciding it either way — a backend that cannot
    /// answer must not be able to cause a rebuild by staying silent about it.
    fn output_activity(&self) -> OutputActivity {
        OutputActivity::Unknown
    }
}

/// Keeps a platform's device-change notifications registered.
///
/// Opaque on purpose: what it holds is a Core Audio listener block on one
/// platform and nothing at all on another, and the caller's only obligation is
/// the same either way — keep it alive.
pub trait DeviceWatch: Send {}

/// The no-op guard for platforms with no device notifications.
impl DeviceWatch for () {}

/// Adapts any [`AudioPlatform`] into the watchdog's [`OutputProbe`].
///
/// A named wrapper rather than a blanket `impl OutputProbe for T:
/// AudioPlatform`, which would collide with every other `OutputProbe`
/// implementation the moment one is written — including the trivial one on
/// [`OutputActivity`] that keeps the tests readable.
#[derive(Debug)]
pub struct PlatformProbe<'a, P: AudioPlatform + ?Sized>(
    /// The platform to ask.
    pub &'a P,
);

impl<P: AudioPlatform + ?Sized> OutputProbe for PlatformProbe<'_, P> {
    fn output_activity(&self) -> OutputActivity {
        AudioPlatform::output_activity(self.0)
    }
}
