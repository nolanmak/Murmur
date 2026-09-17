//! macOS system-audio capture via Core Audio process taps.
//!
//! **This is the only directory in the tree permitted to name macOS types.**
//! CI greps for them everywhere else (docs/REQUIREMENTS.md 6.5).
//!
//! # Why taps and not ScreenCaptureKit
//!
//! A tap needs no Apple-granted entitlement, no kernel extension and no
//! virtual audio device, and it does not require the Screen Recording grant.
//! ScreenCaptureKit cannot capture audio without also running the display
//! pipeline, burns a heavier permission, and re-prompts monthly.
//!
//! # What this costs the user, stated honestly
//!
//! The grant is surfaced as "System Audio Recording Only" inside System
//! Settings → Privacy & Security → **Screen & System Audio Recording** — the
//! screen-recording pane, even though we never get screen access. The
//! `tccutil` service is `AudioCapture` (**not** `SystemAudioCaptureRequests`,
//! which several 2026 write-ups cite and which does not exist).
//!
//! # The permission you cannot observe
//!
//! There is no public API to query or pre-request the system-audio grant. The
//! prompt fires only on the first `AudioDeviceStart` of an aggregate device
//! containing a tap, and **a denial delivers silence indistinguishable from a
//! quiet room**. So [`MacOsPlatform::permission`] reports
//! [`PermissionState::Unobservable`] rather than guessing, and the honest test
//! is a round trip: start a tap, play a tone, see whether samples arrive.
//! `fotw doctor` does exactly that.
//!
//! We deliberately do **not** ship AudioCap's private-TCC-framework probe: it
//! is undocumented and its own users report it unreliable.

mod activity;
mod listeners;
mod mic;
mod tap;

use std::sync::Arc;
use std::sync::mpsc::Receiver;

use crate::device_change::DeviceChangeSignal;
use crate::error::TapError;
use crate::events::{EventBus, PlatformEvent};
use crate::format::FormatRequest;
use crate::ids::{AppInfo, AppRef, DeviceId, DeviceInfo, TapId};
use crate::permission::{Permission, PermissionState, PlatformCaps};
use crate::tap::{AudioPlatform, AudioTap, BoxFuture, DeviceWatch, SystemScope};
use crate::watchdog::OutputActivity;

pub use listeners::{DeviceWatcher, debug_output_report, default_output_uid};
pub use mic::MicTap;
pub use tap::SystemTap;

/// The macOS backend.
#[derive(Debug, Default)]
pub struct MacOsPlatform {
    bus: EventBus,
}

impl MacOsPlatform {
    /// Construct the backend.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish a platform event.
    pub fn emit(&self, event: PlatformEvent) {
        self.bus.emit(event);
    }

    /// Install the Core Audio property listeners for CAP-06, typed.
    ///
    /// [`AudioPlatform::watch_devices`] is the same thing behind the seam's
    /// opaque guard; this returns the concrete [`DeviceWatcher`] for callers
    /// inside the backend.
    ///
    /// Deliberately *not* wired to [`EventBus`]: publishing there takes a
    /// mutex, clones a `String`-bearing event and pushes into an `mpsc`
    /// channel that allocates per message, none of which may happen on the
    /// Core Audio notification thread. The bus stays available for anything
    /// above the seam that wants to fan the change out after the supervisor
    /// has taken it.
    pub fn watch_devices_typed(
        &self,
        signal: Arc<DeviceChangeSignal>,
    ) -> Result<DeviceWatcher, TapError> {
        listeners::watch(signal)
    }
}

impl AudioPlatform for MacOsPlatform {
    fn caps(&self) -> PlatformCaps {
        PlatformCaps {
            system_mix: true,
            // Per-app scoping needs CATapDescription.bundleIDs (macOS 26+) or
            // PID translation. Not wired up yet, and advertising a capability
            // we do not have would make the UI offer a control that silently
            // captures everything.
            app_scoped: false,
            exclude_scope: true,
            emits_silence_when_idle: true,
            needs_consent_for_system: true,
        }
    }

    fn permission(&self, permission: Permission) -> PermissionState {
        match permission {
            // See the module docs: there is no API for this, and pretending
            // otherwise would make onboarding lie.
            Permission::SystemAudio => PermissionState::Unobservable,
            // The mic leg *is* different: `AVCaptureDevice.authorizationStatus`
            // is a real, queryable API, so onboarding handles it up front
            // instead of inferring it from silence (issue #31).
            Permission::Microphone => mic_authorization(),
            Permission::ScreenRecording => PermissionState::NotApplicable,
        }
    }

    fn request_permission(&self, permission: Permission) -> BoxFuture<'static, PermissionState> {
        let state = match permission {
            Permission::SystemAudio => PermissionState::Unobservable,
            Permission::Microphone => mic_authorization(),
            Permission::ScreenRecording => PermissionState::NotApplicable,
        };
        Box::pin(async move { state })
    }

    fn mics(&self) -> Vec<DeviceInfo> {
        // Only the default input for now. Enumerating every device is a
        // settings-UI concern and needs no capture code.
        vec![DeviceInfo::new(
            DeviceId::new("default"),
            "Default input",
            true,
        )]
    }

    fn capturable_apps(&self) -> Vec<AppInfo> {
        Vec::new()
    }

    fn open_mic(
        &self,
        _device: &DeviceId,
        _hint: FormatRequest,
    ) -> Result<Box<dyn AudioTap>, TapError> {
        // A separate device and IOProc from the system tap, never a fused
        // aggregate — see the mic module docs.
        Ok(Box::new(MicTap::default_input()?))
    }

    fn open_system(
        &self,
        scope: SystemScope,
        _hint: FormatRequest,
    ) -> Result<Box<dyn AudioTap>, TapError> {
        let excluded: Vec<AppRef> = match scope {
            SystemScope::DefaultOutputMix => Vec::new(),
            SystemScope::AllExcept(apps) => apps,
            SystemScope::Apps(_) => {
                return Err(TapError::unsupported(
                    "per-app capture requires CATapDescription.bundleIDs (macOS 26+); \
                     not implemented yet",
                ));
            }
        };
        Ok(Box::new(SystemTap::new(
            TapId::system_default(),
            &excluded,
        )?))
    }

    fn events(&self) -> Receiver<PlatformEvent> {
        self.bus.subscribe()
    }

    fn output_activity(&self) -> OutputActivity {
        listeners::output_activity()
    }

    fn watch_devices(
        &self,
        signal: Arc<DeviceChangeSignal>,
    ) -> Result<Box<dyn DeviceWatch>, TapError> {
        Ok(Box::new(listeners::watch(signal)?))
    }
}

/// The microphone authorization status, from the API that actually exists.
///
/// `AVCaptureDevice.authorizationStatus(for: .audio)` is queryable *and*
/// requestable, unlike the system-audio grant — which is why onboarding
/// handles the mic leg up front and the tap leg by round trip (6.3).
///
/// A `Restricted` answer is not a `Denied` one: it means MDM or parental
/// controls, and telling that user to flip a switch they cannot flip is worse
/// than telling them nothing.
fn mic_authorization() -> PermissionState {
    use cidre::av;

    match av::CaptureDevice::authorization_status_for_media_type(av::MediaType::audio()) {
        Ok(av::AuthorizationStatus::Authorized) => PermissionState::Granted,
        Ok(av::AuthorizationStatus::Denied) => PermissionState::Denied,
        Ok(av::AuthorizationStatus::Restricted) => PermissionState::Restricted,
        Ok(av::AuthorizationStatus::NotDetermined) => PermissionState::NotDetermined,
        // The call raised an Objective-C exception, which for this selector
        // means an unsupported media type on this OS. Reporting
        // "not determined" would send onboarding into a request that also
        // cannot work; the honest answer is that we cannot see it.
        Err(_) => PermissionState::Unobservable,
    }
}
