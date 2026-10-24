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
reopening Murmur. The native owner retains the tap and watcher throughout
capture and teardown. The async handle contains only channels: abandoning it
requests cleanup without running native destructors on its executor. No late
transcript is inserted by startup recovery.

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
on main. Device-watcher creation, default-device selection/open, tap start,
Stop and resource destruction all run on the same native owner. Native calls
remain synchronous there; the async handoff has deadlines and cancellation.
Native sleep/format event publication, readiness recovery and physical acceptance
remain required before closing #61.


## Bounded teardown and persistent ownership state

Key release requests cleanup from the existing native owner and waits at most
four seconds for Stop, tap destruction and watcher removal. Escape waits only a
20 ms grace period for quick cleanup, then returns Cancelled while the owner
keeps its resources and lease. Aborting the async task also disconnects that
handle and requests cleanup. The active handle itself performs no native
destruction on the async executor. The native shell uses deferred preparation,
so it creates no native objects before ownership moves to that worker. Existing
prebuilt-tap entry points remain compatible; work a caller performs before
invoking those APIs cannot be bounded by the coordinator.
A blocked native call is not forcibly interrupted and cannot multiply into new
microphone workers through repeated activations.

Only successful Stop followed by resource destruction releases the lease and
returns the operation to Idle. Stop failure leaves capture ownership unconfirmed,
refuses another input and asks the user to reopen Murmur. If Stop succeeded but
watcher cleanup failed, that positive Stop evidence remains distinct: audio is
stopped, cleanup is failed, and another input is still refused until reopening.
This is deliberate: an unconfirmed stop is not permission to claim recording
ended. A timeout whose native cleanup later succeeds clears automatically, but
recording resumes only on a new deliberate activation. A fresh fixture dictation
succeeds after both Stop and watcher-cleanup timeouts resolve.

The native menu and persistent pill distinguish:

| State | Pill / behavior |
| --- | --- |
| Cancelled/timed-out Stop remains pending | Microphone may still be recording; further input preparation is blocked |
| Stop succeeded, watcher removal remains pending | Microphone stopped — finishing cleanup |
| Stop could not be confirmed | Mic may still be recording · reopen Murmur |
| Stop confirmed, cleanup failed | Mic stopped — cleanup failed; reopen Murmur |
| Cleanup confirmed | Normal ready/processing indicator resumes |

These messages outlive normal notice expiry and take priority over transient
results and permission messages. New input, prewarming and permission onboarding
do not bypass unresolved native ownership. A successful Stop is recorded before
watcher destruction, so a subsequent input-route event cannot discard audio that
was already stopped. Callback faults during Stop still reject the result.
A successful provider result cannot be inserted after a cleanup timeout/error.

Fresh behavioral REDs observed Escape waiting 2.02 seconds for injected native
Stop, pending ownership looking Idle after cancellation, and the shortened pill
losing its reopen action. Another RED showed a watcher-cleanup failure discarding
positive Stop evidence. GREEN evidence: 35 capture tests and 14 indicator tests
cover Stop/watcher deadlines, Escape and async task abort, bounded ownership,
no insertion on unconfirmed cleanup, post-Stop route changes, fresh speech after
cleanup, and persistent/clearable honest UI states. Workspace, strict Clippy,
repository, managed-model lifecycle and signed-app build checks pass locally.
All audio/providers/devices here are synthetic fixtures.

### Owned AppKit rendering evidence

The opt-in `microphone_cleanup_preview` example renders the production Pill's
own AppKit views without constructing Shell, registering input hooks, opening a
microphone, checking permissions or reading another app/window. Its temporary
window is nonactivating and invisible to the desktop while its content view is
cached. The process exits after rendering. These images are native view output,
not an image edit or a physical-device recording test:

![Stop remains pending](microphone-cleanup/pending.png)

![Audio stopped, resource cleanup remains pending](microphone-cleanup/stopped.png)

![Unconfirmed Stop retains its recovery action](microphone-cleanup/unconfirmed.png)

![Positive Stop evidence survives cleanup failure](microphone-cleanup/cleanup-failed.png)

```sh
cargo run --example microphone_cleanup_preview -- /tmp/murmur-cleanup-preview
```

The stopped label reflects the backend's successful `AudioTap::stop`; physical
microphone LED/callback quiescence, USB/Bluetooth recovery and sleep/wake behavior
still require signed-app acceptance. These component renders do not establish
those gates or close #61.
