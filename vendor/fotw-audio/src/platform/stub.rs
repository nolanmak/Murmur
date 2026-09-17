//! A backend that honestly refuses to do anything.
//!
//! Every unimplemented OS resolves here. It exists so the seam is exercised on
//! all three platforms from day one — and so an unimplemented platform fails
//! loudly with [`crate::TapError::Unsupported`] rather than recording silence,
//! which is the failure mode this whole project is organised around avoiding.

use std::sync::mpsc::Receiver;

use crate::activity::{ActivityProbe, ActivitySnapshot};
use crate::error::TapError;
use crate::events::{EventBus, PlatformEvent};
use crate::format::FormatRequest;
use crate::ids::{AppInfo, DeviceId, DeviceInfo};
use crate::permission::{Permission, PermissionState, PlatformCaps};
use crate::tap::{AudioPlatform, AudioTap, BoxFuture, SystemScope};

/// A backend with no capabilities.
#[derive(Debug, Default)]
pub struct StubPlatform {
    bus: EventBus,
}

impl StubPlatform {
    /// Construct the stub.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish an event. Used by tests of the layer above.
    pub fn emit(&self, event: PlatformEvent) {
        self.bus.emit(event);
    }
}

impl AudioPlatform for StubPlatform {
    fn caps(&self) -> PlatformCaps {
        PlatformCaps::default()
    }

    fn permission(&self, _permission: Permission) -> PermissionState {
        PermissionState::NotDetermined
    }

    fn request_permission(&self, _permission: Permission) -> BoxFuture<'static, PermissionState> {
        Box::pin(async { PermissionState::NotDetermined })
    }

    fn mics(&self) -> Vec<DeviceInfo> {
        Vec::new()
    }

    fn capturable_apps(&self) -> Vec<AppInfo> {
        Vec::new()
    }

    fn open_mic(
        &self,
        _device: &DeviceId,
        _hint: FormatRequest,
    ) -> Result<Box<dyn AudioTap>, TapError> {
        Err(TapError::unsupported(
            "no microphone backend is compiled in for this platform",
        ))
    }

    fn open_system(
        &self,
        _scope: SystemScope,
        _hint: FormatRequest,
    ) -> Result<Box<dyn AudioTap>, TapError> {
        Err(TapError::unsupported(
            "no system-audio backend is compiled in for this platform",
        ))
    }

    fn events(&self) -> Receiver<PlatformEvent> {
        self.bus.subscribe()
    }
}

/// An unimplemented platform reports a *failure*, never an empty machine.
///
/// Empty means "nothing on this machine is using audio", which the detector
/// above reads as "no meeting in progress". A stub that answered that way
/// would turn a missing backend into a detector that is permanently and
/// silently off — the same class of defect as recording silence.
impl ActivityProbe for StubPlatform {
    fn snapshot(&self) -> Result<ActivitySnapshot, TapError> {
        Err(TapError::unsupported(
            "no activity probe is compiled in for this platform",
        ))
    }
}
