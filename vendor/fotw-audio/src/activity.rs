//! Who is using audio right now — the platform half of meeting detection.
//!
//! Meeting detection (MTG-03) is a conjunction of *(a known conferencing app
//! is running)* **and** *(the microphone is hot)*. Neither fact is knowable
//! above the seam, so this module is the narrow window through which the
//! detector sees the machine. The *policy* — which bundle ids count, how long
//! a signal must hold, when to arm — is deliberately not here: it lives in
//! `fotwd::detect`, where it is testable with no conferencing app installed
//! and no audio device present.
//!
//! # Why an audio-client list rather than a process list
//!
//! On macOS the honest source is Core Audio's own process list
//! (`kAudioHardwarePropertyProcessObjectList`), not `ps`. A process appears
//! there because it has an audio client, and each entry carries
//! `kAudioProcessPropertyIsRunningInput` / `...IsRunningOutput`. That is a
//! much stronger signal than "the Zoom binary is in the process table" — Zoom
//! idles in the background on most installs and would otherwise prompt several
//! times a day, which is precisely the habituation problem [§11.2] names as a
//! *consent* defect rather than a UX one.
//!
//! # The Bluetooth hole, stated plainly
//!
//! [`Transport::mic_activity_is_trustworthy`] exists because a Bluetooth input
//! can report inactive while it is being recorded. For an AirPods user — a
//! large share of the target audience — the mic-hot conjunct can silently
//! never fire. The detector must therefore treat a Bluetooth default input as
//! *no mic signal at all* and fall back to a calendar match, never as
//! "mic is cold, so there is no meeting".

use crate::error::TapError;

/// How an audio device is attached.
///
/// Only the distinctions that change a decision are named; everything else is
/// [`Transport::Unknown`], which is treated as the *untrustworthy* case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Transport {
    /// The built-in microphone or speakers.
    BuiltIn,
    /// USB audio interface or headset.
    Usb,
    /// Thunderbolt / PCI / FireWire interface.
    Thunderbolt,
    /// Classic Bluetooth (AirPods, most headsets).
    Bluetooth,
    /// Bluetooth Low Energy.
    BluetoothLe,
    /// A virtual device — BlackHole, Loopback, a conferencing app's own
    /// driver.
    Virtual,
    /// An aggregate device, including our own private tap aggregate.
    Aggregate,
    /// "Use iPhone as microphone", over a cable.
    ContinuityCaptureWired,
    /// "Use iPhone as microphone", wireless.
    ContinuityCaptureWireless,
    /// HDMI, DisplayPort, AirPlay, AVB, or anything this build does not name.
    Unknown,
}

impl Transport {
    /// Whether "is this device running somewhere" can be believed for
    /// detection purposes.
    ///
    /// **False for every wireless transport.** A Bluetooth microphone can
    /// report inactive to `kAudioDevicePropertyDeviceIsRunningSomewhere` while
    /// a call is in progress (issue #22). A detector that treated that as
    /// "the mic is cold" would never fire for AirPods users and would never
    /// say why, so the honest answer is that the signal is *absent*, not
    /// negative.
    ///
    /// `Unknown` is untrustworthy on the same reasoning: an unrecognised
    /// transport is one whose reporting behaviour nobody here has checked.
    /// Erring this way costs a missed prompt — the user can still press
    /// record. Erring the other way costs a detector that is confidently
    /// wrong.
    #[must_use]
    pub const fn mic_activity_is_trustworthy(self) -> bool {
        matches!(
            self,
            Self::BuiltIn | Self::Usb | Self::Thunderbolt | Self::ContinuityCaptureWired
        )
    }

    /// Whether this is a virtual or aggregate device rather than hardware.
    ///
    /// Worth surfacing in onboarding: a user whose default input is a virtual
    /// device has routing software in the path, and both detection and capture
    /// behave differently there.
    #[must_use]
    pub const fn is_synthetic(self) -> bool {
        matches!(self, Self::Virtual | Self::Aggregate)
    }
}

/// One process that holds an audio client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioClient {
    /// The process id. Not stable across a restart of that app.
    pub pid: u32,
    /// The process's bundle identifier, when it has one. Command-line tools
    /// and helpers often do not.
    pub bundle_id: Option<String>,
    /// The process is pulling from an input device — i.e. holding a
    /// microphone.
    pub running_input: bool,
    /// The process is pushing to an output device.
    pub running_output: bool,
}

impl AudioClient {
    /// A client that is doing nothing.
    #[must_use]
    pub fn new(pid: u32, bundle_id: Option<&str>) -> Self {
        Self {
            pid,
            bundle_id: bundle_id.map(ToOwned::to_owned),
            running_input: false,
            running_output: false,
        }
    }

    /// Set [`AudioClient::running_input`].
    #[must_use]
    pub const fn with_input(mut self, running: bool) -> Self {
        self.running_input = running;
        self
    }

    /// Set [`AudioClient::running_output`].
    #[must_use]
    pub const fn with_output(mut self, running: bool) -> Self {
        self.running_output = running;
        self
    }

    /// Whether this process is doing any audio IO at all.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.running_input || self.running_output
    }
}

/// The default input device, as detection sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDevice {
    /// Human-readable name, for onboarding copy and logs.
    pub name: String,
    /// How it is attached. Decides whether `running_somewhere` means anything.
    pub transport: Transport,
    /// `kAudioDevicePropertyDeviceIsRunningSomewhere`: some process on the
    /// machine has this device running. **Only meaningful when
    /// [`Transport::mic_activity_is_trustworthy`].**
    pub running_somewhere: bool,
}

impl InputDevice {
    /// Construct a device record.
    #[must_use]
    pub fn new(name: impl Into<String>, transport: Transport, running_somewhere: bool) -> Self {
        Self {
            name: name.into(),
            transport,
            running_somewhere,
        }
    }
}

/// Everything the detector gets to see about the machine, at one instant.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ActivitySnapshot {
    /// Processes holding an audio client.
    pub clients: Vec<AudioClient>,
    /// The default input device, if there is one. `None` on a machine with no
    /// microphone — which is a real configuration, not an error.
    pub default_input: Option<InputDevice>,
}

impl ActivitySnapshot {
    /// The client for a bundle id, if it holds one.
    #[must_use]
    pub fn client(&self, bundle_id: &str) -> Option<&AudioClient> {
        self.clients
            .iter()
            .find(|c| c.bundle_id.as_deref() == Some(bundle_id))
    }

    /// Every process currently holding an input device.
    pub fn input_holders(&self) -> impl Iterator<Item = &AudioClient> {
        self.clients.iter().filter(|c| c.running_input)
    }

    /// Every process holding an input device except `pid`.
    ///
    /// Our own recording holds the microphone. Counting that as evidence of a
    /// meeting would make the detector re-arm off its own capture.
    pub fn input_holders_excluding(&self, pid: u32) -> impl Iterator<Item = &AudioClient> {
        self.input_holders().filter(move |c| c.pid != pid)
    }

    /// Whether the mic-hot signal can be believed at all right now.
    ///
    /// False when there is no input device, and false for every wireless
    /// transport — see [`Transport::mic_activity_is_trustworthy`].
    #[must_use]
    pub fn mic_is_trustworthy(&self) -> bool {
        self.default_input
            .as_ref()
            .is_some_and(|d| d.transport.mic_activity_is_trustworthy())
    }
}

/// A source of [`ActivitySnapshot`]s.
///
/// Separate from [`crate::AudioPlatform`] on purpose: detection is a read-only
/// question about the machine, needs no tap, no grant and no teardown, and a
/// backend that cannot answer it should not have to pretend it can open a tap.
pub trait ActivityProbe: Send + Sync {
    /// Sample the machine.
    ///
    /// # Errors
    ///
    /// Whatever the platform reports. An error is **not** the same as an empty
    /// snapshot: empty means "nothing is using audio", which the detector
    /// reads as "no meeting". A probe that failed must say so, or a broken
    /// probe disables detection silently and forever.
    fn snapshot(&self) -> Result<ActivitySnapshot, TapError>;
}
