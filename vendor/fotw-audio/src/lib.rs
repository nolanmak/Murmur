//! `fotw-audio` — the platform seam for FlyOnTheWall's capture layer.
//!
//! This crate is the *only* place that is allowed to know how a given
//! operating system produces audio. Everything above it (rings, resampler,
//! AEC, muxer, STT) sees nothing but [`AudioPlatform`], [`AudioTap`] and
//! [`FrameSink`]. See docs/REQUIREMENTS.md 6.5.
//!
//! The five non-negotiable seam rules, and where they live in this crate:
//!
//! 1. [`FormatRequest`] is a *hint*; [`AudioTap::format`] after
//!    [`AudioTap::start`] is truth. [`AudioTap::format_is_authoritative`]
//!    makes that observable, and [`TapSession`] re-reads the format after
//!    every start so a mid-session rate change never forces a rebuild above
//!    the seam.
//! 2. Two logical tracks always, never pre-mixed: there is a separate tap for
//!    the mic leg ([`AudioPlatform::open_mic`]) and the system leg
//!    ([`AudioPlatform::open_system`]), and no API here mixes them.
//! 3. All timestamps are stamped at the tap boundary from one process-wide
//!    monotonic clock — see [`clock::host_ns`].
//! 4. [`PlatformCaps::emits_silence_when_idle`] tells the layer above whether
//!    it has to synthesise the gaps itself.
//! 5. [`AudioPlatform::events`] exists from day one.
//!
//! # Surviving a tap that dies without saying so (CAP-05, CAP-06)
//!
//! A Core Audio process tap can keep reporting success while delivering
//! nothing, or nothing but zeros, and a device change mid-meeting is the
//! commonest way it happens. Three modules answer that, and none of them names
//! an operating system:
//!
//! * [`watchdog`] decides *that* a tap is broken, from counters and an
//!   injected clock. Read its module docs before changing a threshold — the
//!   two conditions it distinguishes are not the same condition, and the
//!   corroboration signal the spec relies on is weaker than the spec assumes.
//! * [`device_change`] carries a hardware change from a platform notification
//!   thread that may not allocate to a supervisor thread that may.
//! * [`supervisor`] does the full teardown and rebuild, and reports what the
//!   recording lost as a [`CaptureGap`] the writer can act on.
//!
//! # Testing without an audio device
//!
//! [`FileAudioSource`] is an [`AudioTap`] that replays a WAV fixture — in real
//! time, or at a speed multiplier so a 90-minute meeting runs in under two
//! minutes of CI. [`FilePlatform`] serves those taps through the ordinary
//! [`AudioPlatform`] interface, so the whole pipeline can be driven from
//! fixtures with no device, no GUI and no TCC grant (docs/REQUIREMENTS.md
//! 5.6). [`testing`] holds the fakes shared with the crates above.
//!
//! # Platform-type containment
//!
//! No operating-system type may appear in this crate's public API, and
//! macOS-specific symbols may not appear outside `src/platform/macos/`. CI
//! greps for them; see docs/REQUIREMENTS.md 6.5 "Enforcement".

#![warn(missing_docs)]

pub mod activity;
pub mod clock;
pub mod device_change;
mod error;
mod events;
mod format;
mod frames;
mod ids;
mod permission;
pub mod platform;
mod session;
pub mod supervisor;
mod tap;
pub mod testing;
pub mod watchdog;
pub mod wav;

pub use crate::activity::{ActivityProbe, ActivitySnapshot, AudioClient, InputDevice, Transport};
pub use crate::device_change::{DeviceChangeKind, DeviceChangeSignal, DeviceChanges};
pub use crate::error::TapError;
pub use crate::events::{EventBus, PlatformEvent};
pub use crate::format::{FormatRequest, SampleFormat, StreamFormat};
pub use crate::frames::{CaptureTimestamp, FrameFlags, FrameSink};
pub use crate::ids::{AppInfo, AppRef, DeviceId, DeviceInfo, TapId};
pub use crate::permission::{Permission, PermissionState, PlatformCaps};
pub use crate::platform::file::{FileAudioSource, FilePlatform, ReplaySpeed};
pub use crate::session::TapSession;
pub use crate::supervisor::{
    CaptureGap, CaptureSupervisor, GapKind, HealthEvent, SupervisorConfig,
};
pub use crate::tap::{AudioPlatform, AudioTap, BoxFuture, DeviceWatch, PlatformProbe, SystemScope};
pub use crate::watchdog::{
    ActivityCounters, OutputActivity, TapActivity, Watchdog, WatchdogConfig,
};
