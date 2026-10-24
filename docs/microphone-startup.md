# Bounded microphone startup

The capture coordinator adapts PR #53 (97fe25cf43cc94734d45a4b33e879e259c97e10f)
to the current local/cloud pipeline. The native shell defers device-watcher
registration, default-device selection/open and `AudioTap::start` to one owned
worker, after profile/provider preflight. All these native preparation calls
share a four-second deadline; Escape and release before samples arrive stop waiting.
A release after samples arrive still drains captured audio normally.

The native shell retains one `MicrophoneStartup` across attempts. If CoreAudio
stays blocked after timeout/cancellation, subsequent attempts refuse to spawn
another preparer until the old one returns and its abandoned tap and watcher
are cleaned up. The lease stays with the owned capture guard through Stop and
watcher destruction, so slow cleanup cannot multiply native operations either.
A preparation that returns after cancellation/release/timeout is closed without
starting late capture. This cannot forcibly interrupt an OS call. A permanently blocked OS call may require reconnecting the device or
reopening Murmur. A returned tap travels inside an RAII guard, so abandoning
the handoff also closes it. No late transcript is inserted by startup recovery.

A fresh preparation regression observed Escape waiting 2.01 seconds for an
injected blocked open before the coordinator was reached. It now returns promptly.
All 29 capture tests pass, covering preparation timeout, Escape/release, no late
start, repeated-attempt backpressure, lease retention through blocked Stop or
watcher destruction, preflight rejection before preparation, and fresh reuse.
A second fresh RED started a device once after preparation had already reported
an input-route change; the worker now checks that signal before native start,
without waiting for an async observer tick. Existing capture faults, stereo
conversion, route/stop boundaries, vocabulary snapshots and fallback remain covered.
The fixture has no physical microphone or cloud access. Native unplug/replug,
Bluetooth and sleep/wake acceptance for #61 remain required; this change does
not implement the whole device-recovery state machine or claim that OS stop
calls can be interrupted.


## Capture faults cannot become completed dictation

The callback now latches its first fault: queue overflow, device disappearance,
explicit discontinuity, non-finite samples, changed stream layout or a generic
tap error. Later
callbacks cannot clear the fault or append more audio. Valid silent buffers
still count as delivery. Invalid samples are rejected before the resampler;
otherwise NaN input can panic in its FFT processing.

Startup completion, the capture loop, stop and provider completion all check
that fault. Escape wins over a provider result too. A successful provider
result before capture has stopped is refused, rather than inserting while the
microphone is still open. The returned error names the failure without exposing
speech. Dropping the attempt closes its owned tap and provider future; a new
deliberate attempt receives fresh state.

Three behavioral tests failed before the fix: discontinuous audio incorrectly
returned a complete transcript, a device error during stop lost to a fast
provider completion, and non-finite samples panicked inside the resampler.
All 16 capture tests now pass, including those regressions, valid silent buffers
and a successful fresh attempt after a discontinuity. Fixtures use local fake
providers and taps, with no physical microphone or cloud credentials.

This is part of #61, not the full recovery implementation. Native route-change
notifications, sleep-aware stall detection, bounded background readiness retries,
sustained-health reset and physical unplug/Bluetooth/sleep testing remain open.
The native tap must actually report a fault for this callback path to observe it;
missing callbacks still use the existing three-second stall timeout. No partial
transcript is automatically inserted and no background recording is started.

The shared backend's typed `StreamFormatChanged` fault now asks for a new
dictation instead of reporting a generic failure. Its added regression failed
with “Microphone failure; no text inserted” before that mapping, then passed with
the format-specific instruction. Full workspace (207 tests), strict Clippy,
repository boundaries, managed-engine lifecycle tests and the stable-identity
signed development bundle passed locally at this extraction.


## Per-attempt device notifications

The macOS shell now registers the shared CoreAudio device watcher before opening
the microphone and holds it until the attempt completes. Registration failure
refuses capture with a retry message. Each attempt owns a fresh notification
signal; an old watcher or delayed callback cannot invalidate a later attempt.

Default-input changes cancel pending startup or active capture. The startup
worker retains its existing cleanup/lease guarantees if the OS call returns
late. Output-only and general device-list notifications do not cancel microphone
dictation. A route change after capture has stopped does not discard a complete
utterance while transcription drains. No automatic recording restart occurs.

The original default-input test returned a successful transcript and failed its
cancellation assertion. After wiring, a separate boundary test exposed excessive
cancellation after stop; restricting notification checks to active capture fixed
it. All 20 capture tests now pass, including input/output distinction, a fresh
attempt after a stale notification, completed-audio preservation and prompt
startup cancellation with late-tap cleanup.

The policy also recognizes explicit StreamFormat/WillSleep/DidWake events, but
the shared native watcher currently publishes only default-device/device-list
changes. Native format and sleep event sources, sleep-aware stall clocks,
background readiness recovery and physical device QA remain unfinished under
#61. Synthetic event tests do not establish those native acceptance gates.

The route change extraction retains the five-argument startup API, independent
progress/startup state, panic containment and stereo frame preservation already
on main. Device-watcher creation, default-device selection/open and tap start
now run inside the bounded preparation worker. OS calls remain synchronous on
that worker. Native teardown still needs a separate deadline/cancellable handoff;
this change retains the lease through cleanup but does not make Stop interruptible.
Native sleep/format event publication, readiness recovery and physical acceptance
remain required before closing #61.
