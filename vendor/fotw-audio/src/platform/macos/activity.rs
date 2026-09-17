//! The macOS activity probe, from Core Audio's own process list.
//!
//! `kAudioHardwarePropertyProcessObjectList` is the right source for meeting
//! detection and `ps` is the wrong one. A process appears in this list because
//! it holds an audio client, and each entry carries
//! `kAudioProcessPropertyIsRunningInput` / `...IsRunningOutput`. "Zoom is in
//! the process table" is nearly worthless — Zoom idles in the background on
//! most installs. "Zoom holds the microphone" is close to a meeting.
//!
//! Nothing here needs a TCC grant: the process list, the bundle ids and the
//! running flags are all readable without the audio-capture permission. That
//! matters, because detection has to work *before* onboarding succeeds, and a
//! detector that needed the grant it exists to prompt for would be circular.
//!
//! # What this cannot see
//!
//! - A process with no bundle id (command-line tools, some helpers) is
//!   reported with `bundle_id: None`. The detector treats those as anonymous
//!   mic holders — they can satisfy "the mic is hot", never "a conferencing
//!   app is running".
//! - Browser meetings render audio from helper processes. Chrome's helpers
//!   report bundle ids like `com.google.Chrome.helper`, which the catalog
//!   above the seam matches by prefix.
//!
//! # Measured on macOS 26.3, and it matters for the policy above
//!
//! On an idle MacBook Air with nothing playing, this list had **43 entries**,
//! and `com.apple.CoreSpeech` reported `IsRunningInput = true` while the
//! built-in microphone's `DeviceIsRunningSomewhere` was simultaneously
//! `false`. Sampled again a few minutes later, CoreSpeech's flag had gone —
//! so Siri's listening path raises it intermittently, unpredictably, and
//! without any call being in progress.
//!
//! Two consequences, both load-bearing:
//!
//! 1. "Some process holds the input" is **not** a usable mic-hot signal: it
//!    goes true on a stock Mac with nobody talking to anything. The detector
//!    must ask whether *the conferencing app itself* holds the input.
//! 2. The process-level and device-level flags genuinely disagree, so neither
//!    can be treated as the ground truth for the other. Both are reported here
//!    and the policy above decides.

use cidre::core_audio::{DeviceTransportType, Process, PropSelector, System};

use crate::activity::{ActivityProbe, ActivitySnapshot, AudioClient, InputDevice, Transport};
use crate::error::TapError;
use crate::platform::macos::MacOsPlatform;

impl ActivityProbe for MacOsPlatform {
    fn snapshot(&self) -> Result<ActivitySnapshot, TapError> {
        let processes = Process::list().map_err(|e| {
            TapError::platform(format!("could not read the audio process list: {e:?}"))
        })?;

        let clients = processes
            .iter()
            .filter_map(|p| {
                // A process that vanishes between the list call and the
                // property read is normal, not an error: it is dropped rather
                // than failing the whole snapshot, because one dying helper
                // must not blind the detector.
                let pid = u32::try_from(p.pid().ok()?).ok()?;
                Some(AudioClient {
                    pid,
                    // Observed on macOS 26.3: some processes answer the bundle-id
                    // property with an *empty* string rather than an error. An
                    // empty id would match a catalog entry built from `""` and
                    // is indistinguishable from "no bundle id", so normalise it.
                    bundle_id: p
                        .bundle_id()
                        .ok()
                        .map(|s| s.to_string())
                        .filter(|s| !s.is_empty()),
                    running_input: p.is_running_input().unwrap_or(false),
                    running_output: p.is_running_output().unwrap_or(false),
                })
            })
            .collect();

        Ok(ActivitySnapshot {
            clients,
            default_input: default_input(),
        })
    }
}

/// The default input device, or `None` on a machine with no microphone.
///
/// A machine with no input device is a real configuration (a Mac mini with no
/// headset, a display-only setup), so this is an `Option` rather than an
/// error.
fn default_input() -> Option<InputDevice> {
    let device = System::default_input_device().ok()?;
    let name = device
        .name()
        .map(|s| s.to_string())
        .unwrap_or_else(|_| "Unknown input".to_owned());
    let transport = device
        .transport_type()
        .map_or(Transport::Unknown, transport);

    // NOT `Device::is_running()`, which is `kAudioDevicePropertyDeviceIsRunning`
    // — "running in *this* process". The detector needs
    // `kAudioDevicePropertyDeviceIsRunningSomewhere`: some process on the
    // machine has the mic open. Confusing the two yields a mic-hot signal that
    // is true exactly when we are already recording.
    let running_somewhere = device
        .bool_prop(&PropSelector::DEVICE_IS_RUNNING_SOMEWHERE.global_addr())
        .unwrap_or(false);

    Some(InputDevice {
        name,
        transport,
        running_somewhere,
    })
}

/// Map Core Audio's four-character transport code onto the seam's enum.
///
/// Everything not named maps to [`Transport::Unknown`], which the layer above
/// treats as "the mic-hot signal cannot be believed" rather than as "the mic
/// is cold".
fn transport(t: DeviceTransportType) -> Transport {
    match t {
        DeviceTransportType::BUILT_IN => Transport::BuiltIn,
        DeviceTransportType::USB => Transport::Usb,
        DeviceTransportType::THUNDERBOLT
        | DeviceTransportType::PCI
        | DeviceTransportType::FIRE_WIRE => Transport::Thunderbolt,
        DeviceTransportType::BLUETOOTH => Transport::Bluetooth,
        DeviceTransportType::BLUETOOTH_LE => Transport::BluetoothLe,
        DeviceTransportType::VIRTUAL => Transport::Virtual,
        DeviceTransportType::AGGREGATE => Transport::Aggregate,
        DeviceTransportType::CONTINUITY_CAPTURE_WIRED => Transport::ContinuityCaptureWired,
        DeviceTransportType::CONTINUITY_CAPTURE_WIRELESS => Transport::ContinuityCaptureWireless,
        _ => Transport::Unknown,
    }
}
