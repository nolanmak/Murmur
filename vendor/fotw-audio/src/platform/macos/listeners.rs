//! Core Audio property listeners, and the process-output probe.
//!
//! Two jobs, both of which exist to feed the platform-free logic above:
//! telling the supervisor that the hardware moved, and answering whether
//! anything on the machine is actually playing.
//!
//! # What the listener block is allowed to do
//!
//! `AudioObjectAddPropertyListenerBlock` delivers on the dispatch queue it is
//! registered with. That is not the IOProc's real-time thread, but it is a
//! thread the HAL is waiting on, and it is shared with every other
//! notification in the process: blocking it delays device notifications
//! system-wide, and a `malloc` there can block on the allocator lock behind
//! any other thread. So the block reads a `u32` selector out of a
//! caller-owned array and performs two atomic read-modify-writes into
//! [`DeviceChangeSignal`]. It does not allocate, lock, log, or make a single
//! decision. Everything that decides what to *do* runs on the supervisor's own
//! thread — see [`crate::supervisor`].
//!
//! One block is registered against all three addresses rather than three
//! blocks against one each. The HAL hands the block the addresses that
//! actually changed, so the mapping stays a lookup rather than three
//! closures each carrying their own captured constant.
//!
//! # Why the probe skips our own process
//!
//! The corroboration for a silence stall is "some *other* process is rendering
//! output". We hold a running aggregate device with a tap in it for the whole
//! meeting, so if we counted ourselves the evidence would be permanently true,
//! the corroboration would be vacuous, and the watchdog would rebuild the tap
//! on a timer through every quiet meeting — the exact failure the
//! corroboration rule exists to prevent.
//!
//! Excluding ourselves is necessary and, as [`crate::watchdog`] documents from
//! a measurement taken here, not sufficient: any *other* process with an
//! output stream open satisfies the property while producing nothing audible.

use std::sync::Arc;

use cidre::{
    arc,
    core_audio::{self as ca, PropAddr, PropListenerBlock, PropSelector, System},
    dispatch,
};

use crate::device_change::{DeviceChangeKind, DeviceChangeSignal};
use crate::error::TapError;
use crate::watchdog::OutputActivity;

/// The three system-object properties issue #26 names.
const WATCHED: [(PropSelector, DeviceChangeKind); 3] = [
    (
        PropSelector::HW_DEFAULT_OUTPUT_DEVICE,
        DeviceChangeKind::DefaultOutput,
    ),
    (
        PropSelector::HW_DEFAULT_INPUT_DEVICE,
        DeviceChangeKind::DefaultInput,
    ),
    (PropSelector::HW_DEVICES, DeviceChangeKind::DeviceList),
];

/// Live property listeners. Dropping this removes them.
///
/// It must be *held* for as long as the recording runs. A listener whose
/// registration is dropped stops firing with no diagnostic anywhere, and the
/// symptom is a recording that dies on the next AirPods switch and never
/// recovers — indistinguishable from never having written this module.
pub struct DeviceWatcher {
    block: arc::R<PropListenerBlock>,
    queue: arc::R<dispatch::Queue>,
    registered: Vec<PropAddr>,
}

// The listener block and its queue are handed to Core Audio and are not
// touched by this type after registration; the only thing it does with them
// later is unregister on drop. Same reasoning as `SystemTap`'s `Running`.
unsafe impl Send for DeviceWatcher {}

impl crate::tap::DeviceWatch for DeviceWatcher {}

impl std::fmt::Debug for DeviceWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceWatcher")
            .field("registered", &self.registered.len())
            .finish()
    }
}

impl Drop for DeviceWatcher {
    fn drop(&mut self) {
        for addr in &self.registered {
            // Nothing useful to do about a failure here: the process is
            // tearing the watcher down either way, and the alternative to
            // ignoring it is a panic in a `Drop`.
            let _ =
                System::OBJ.remove_prop_listener_block(addr, Some(&self.queue), &mut self.block);
        }
    }
}

/// Start listening for device changes, raising into `signal`.
///
/// The returned watcher must be kept alive for the length of the session.
pub fn watch(signal: Arc<DeviceChangeSignal>) -> Result<DeviceWatcher, TapError> {
    // A dedicated serial queue rather than `None`. Passing `None` asks the HAL
    // to deliver on the main run loop, and a daemon with no run loop pumping
    // would then never see a notification at all — a failure that looks
    // exactly like the listener not being registered.
    let queue = dispatch::Queue::serial_with_ar_pool();

    let mut block = cidre::blocks::EscBlock::new2(move |n: u32, addrs: *const PropAddr| {
        if addrs.is_null() || n == 0 {
            return;
        }
        // SAFETY: Core Audio passes `n` valid `PropAddr`s for the duration of
        // this call. Read only, never retained.
        let changed = unsafe { std::slice::from_raw_parts(addrs, n as usize) };
        for addr in changed {
            for (selector, kind) in WATCHED {
                if addr.selector == selector {
                    signal.raise(kind);
                }
            }
        }
    });

    let mut registered = Vec::with_capacity(WATCHED.len());
    for (selector, _) in WATCHED {
        let addr = selector.global_addr();
        System::OBJ
            .add_prop_listener_block(&addr, Some(&queue), &mut block)
            .map_err(|e| {
                TapError::platform(format!(
                    "AudioObjectAddPropertyListenerBlock({selector:?}) failed: {e:?}"
                ))
            })?;
        registered.push(addr);
    }

    Ok(DeviceWatcher {
        block,
        queue,
        registered,
    })
}

/// Whether any *other* process is currently rendering output audio.
///
/// `kAudioHardwarePropertyProcessObjectList` +
/// `kAudioProcessPropertyIsRunningOutput`, which is the corroboration
/// docs/REQUIREMENTS.md 6.4 specifies for the silence rule.
///
/// Costs a process-object walk plus one property read per process, so it is
/// deliberately only reachable from the watchdog's silence path and never from
/// its healthy one. It returns on the first process that answers yes.
#[must_use]
pub fn output_activity() -> OutputActivity {
    let Ok(processes) = System::processes() else {
        // The list could not be read. That is not evidence of silence *or* of
        // playback, and reporting either would make the watchdog act on a
        // guess.
        return OutputActivity::Unknown;
    };

    let ours = i32::try_from(std::process::id()).unwrap_or(-1);
    for process in processes {
        if process.pid().is_ok_and(|pid| pid == ours) {
            continue;
        }
        if process.is_running_output().unwrap_or(false) {
            return OutputActivity::Active;
        }
    }
    OutputActivity::Idle
}

/// A human-readable dump of which processes claim to be rendering output.
///
/// Support diagnostic: "the watchdog says something is playing and I cannot
/// hear anything" is otherwise unanswerable from a bug report.
#[must_use]
pub fn debug_output_report() -> String {
    let Ok(processes) = System::processes() else {
        return "process list unavailable".to_owned();
    };
    let ours = i32::try_from(std::process::id()).unwrap_or(-1);
    let mut lines = Vec::new();
    for process in processes {
        let pid = process.pid().unwrap_or(-1);
        let running_out = process.is_running_output().unwrap_or(false);
        let running_in = process.is_running_input().unwrap_or(false);
        if !running_out && !running_in {
            continue;
        }
        let bundle = process
            .bundle_id()
            .map(|b| b.to_string())
            .unwrap_or_else(|_| "<none>".to_owned());
        lines.push(format!(
            "pid {pid}{} out={running_out} in={running_in} {bundle}",
            if pid == ours { " (us)" } else { "" }
        ));
    }
    if lines.is_empty() {
        return "no process is running audio IO".to_owned();
    }
    lines.join("\n")
}

/// The default output device's UID, for logs and gap reasons.
///
/// Best effort: a device that vanished between the notification and this call
/// is exactly the situation being reported, so failure is normal here and is
/// reported as `None` rather than as an error.
#[must_use]
pub fn default_output_uid() -> Option<String> {
    let device = ca::System::default_output_device().ok()?;
    Some(device.uid().ok()?.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mapping has to be exhaustive and collision-free, and it is a table
    /// rather than a `match`, so nothing else checks it.
    #[test]
    fn every_watched_selector_maps_to_a_distinct_kind() {
        let mut kinds = Vec::new();
        for (selector, kind) in WATCHED {
            assert!(!kinds.contains(&kind), "{kind} is mapped twice");
            kinds.push(kind);
            assert_ne!(selector, PropSelector::WILDCARD);
        }
        assert_eq!(kinds.len(), 3);
    }

    /// Runs against the real HAL. It must answer *something* without hanging
    /// or panicking on a machine with no audio devices at all, which is what
    /// a CI runner is.
    #[test]
    fn the_output_probe_answers_on_this_machine() {
        let answer = output_activity();
        assert!(matches!(
            answer,
            OutputActivity::Active | OutputActivity::Idle | OutputActivity::Unknown
        ));
    }

    /// Registering and unregistering must both work, and the watcher must be
    /// droppable without leaving a listener behind. A leaked registration
    /// whose block has been freed is a use-after-free in the HAL's hands.
    #[test]
    fn listeners_register_and_unregister_cleanly() {
        let signal = DeviceChangeSignal::new();
        for _ in 0..3 {
            let watcher = watch(Arc::clone(&signal)).expect("listeners must install");
            drop(watcher);
        }
        // Nothing has changed, so nothing should have been raised. (If the
        // user happens to plug in a device during this test it will raise,
        // which is why this asserts nothing about the value.)
        let _ = signal.peek();
    }
}
