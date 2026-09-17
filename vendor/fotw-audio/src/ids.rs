//! Stable identities for taps, devices and capturable apps.

use std::fmt;

use crate::format::StreamFormat;

/// The identity of a tap, in the wire shape from docs/REQUIREMENTS.md 6.5:
/// `mic:<uid>`, `system:default`, `system:app:<key>`.
///
/// It is a string on purpose. It is written into the per-stream JSONL index
/// (CAP-03) and into the store, so it has to survive a restart, a schema
/// migration and a platform port without carrying an OS handle.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TapId(String);

impl TapId {
    /// The mic leg for a device UID.
    pub fn mic(device_uid: impl AsRef<str>) -> Self {
        Self(format!("mic:{}", device_uid.as_ref()))
    }

    /// The system leg for the default output mix.
    #[must_use]
    pub fn system_default() -> Self {
        Self("system:default".to_owned())
    }

    /// The system leg scoped to one app (bundle id on macOS, pid elsewhere).
    pub fn system_app(key: impl AsRef<str>) -> Self {
        Self(format!("system:app:{}", key.as_ref()))
    }

    /// Wrap an already-formatted id, e.g. one read back from the store.
    pub fn from_raw(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// The id as a string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True for the microphone leg.
    #[must_use]
    pub fn is_mic(&self) -> bool {
        self.0.starts_with("mic:")
    }

    /// True for any system-audio leg, scoped or not.
    #[must_use]
    pub fn is_system(&self) -> bool {
        self.0.starts_with("system:")
    }
}

impl fmt::Display for TapId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A platform's identifier for an input device.
///
/// Opaque and platform-defined (a Core Audio device UID, a WASAPI endpoint id,
/// a PipeWire node name). Never parse it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceId(String);

impl DeviceId {
    /// Wrap a platform identifier.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The identifier as a string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An input device, as offered to the user in a picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Platform identifier, to hand back to
    /// [`crate::AudioPlatform::open_mic`].
    pub id: DeviceId,
    /// Human-readable name.
    pub name: String,
    /// Whether this is the system default input right now.
    pub is_default: bool,
    /// The device's currently configured format, if the platform will say.
    ///
    /// A *hint* for display and for sizing buffers — not authoritative. Only
    /// [`crate::AudioTap::format`] after `start()` is.
    pub nominal_format: Option<StreamFormat>,
}

impl DeviceInfo {
    /// Construct a device record with no format hint.
    pub fn new(id: DeviceId, name: impl Into<String>, is_default: bool) -> Self {
        Self {
            id,
            name: name.into(),
            is_default,
            nominal_format: None,
        }
    }

    /// Attach a non-authoritative format hint.
    #[must_use]
    pub fn with_nominal_format(mut self, format: StreamFormat) -> Self {
        self.nominal_format = Some(format);
        self
    }
}

/// How an app is named for scoped capture.
///
/// Which variant a platform accepts is a platform fact: macOS 26+ takes bundle
/// ids via `CATapDescription`, macOS 14.4–15 needs pid translation, Windows
/// process loopback takes a pid, PipeWire matches on node properties.
/// [`crate::PlatformCaps::app_scoped`] says whether any of it is available.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum AppRef {
    /// A bundle identifier, e.g. `us.zoom.xos`.
    BundleId(String),
    /// A process id. Not stable across an app restart.
    Pid(u32),
    /// An executable or node name, for platforms with neither of the above.
    Name(String),
}

impl AppRef {
    /// The key used in a [`TapId::system_app`].
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Self::BundleId(id) | Self::Name(id) => id.clone(),
            Self::Pid(pid) => pid.to_string(),
        }
    }
}

impl fmt::Display for AppRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BundleId(id) => write!(f, "bundle:{id}"),
            Self::Pid(pid) => write!(f, "pid:{pid}"),
            Self::Name(name) => write!(f, "name:{name}"),
        }
    }
}

/// A capturable app, as offered in the "meeting apps only" picker (CAP-13).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppInfo {
    /// How to ask for this app in a [`crate::SystemScope`].
    pub app: AppRef,
    /// Human-readable name.
    pub name: String,
    /// Whether the app is producing output right now. On macOS this is
    /// `kAudioProcessPropertyIsRunningOutput`, and it is also the signal the
    /// zero-buffer watchdog needs (CAP-05).
    pub is_playing_audio: bool,
}

impl AppInfo {
    /// Construct an app record.
    pub fn new(app: AppRef, name: impl Into<String>, is_playing_audio: bool) -> Self {
        Self {
            app,
            name: name.into(),
            is_playing_audio,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tap_ids_classify_themselves() {
        assert!(TapId::mic("uid").is_mic());
        assert!(!TapId::mic("uid").is_system());
        assert!(TapId::system_default().is_system());
        assert!(TapId::system_app("us.zoom.xos").is_system());
    }

    #[test]
    fn app_ref_keys_are_stable() {
        assert_eq!(AppRef::BundleId("us.zoom.xos".into()).key(), "us.zoom.xos");
        assert_eq!(AppRef::Pid(42).key(), "42");
        assert_eq!(
            TapId::system_app(AppRef::Pid(42).key()).as_str(),
            "system:app:42"
        );
    }
}
