# Microphone ownership and recovery

Murmur cancels interrupted dictation and can check whether the default input is
available again. Recovery never resumes recording or inserts an old transcript.
Start a new dictation deliberately after the microphone becomes available.
Physical USB/Bluetooth, sleep/wake and next-field acceptance remain pending in
[#61](https://github.com/nolanmak/Murmur/issues/61) and [native QA](QA.md).

## Bounded startup and teardown

The macOS shell retains one `MicrophoneStartup` across attempts. After
profile/provider preflight, one native owner registers the device watcher,
selects/opens the default input and starts the tap. Preparation shares a
four-second handoff deadline. Escape or release before samples arrive stops
waiting; release after samples arrive drains captured audio normally.
These bounds cover the shell's deferred preparation; native work a caller does
before invoking a prebuilt-tap API is outside the coordinator's deadline.

A late preparation closes its abandoned tap and watcher without starting capture.
A blocked OS call keeps its lease until it returns and cleanup finishes. Repeated
activations cannot create another preparer or compete with that owner. The async
handle owns channels, so abandoning it requests cleanup without running native
destructors on its executor. Native calls cannot be forcibly interrupted; a
permanently blocked call can require reconnecting the device or reopening Murmur.

Key release waits at most four seconds for Stop and resource destruction. Escape
allows a 20 ms cleanup grace period, then returns Cancelled while the native owner
retains its resources. Aborting the async task also requests cleanup. A provider
result cannot be inserted after a capture fault or cleanup timeout/error.

## Capture faults and device changes

The callback latches the first queue overflow, missing device, discontinuity,
non-finite sample, changed layout or tap error. Later callbacks cannot clear it
or append more audio. Valid silent buffers still count as delivery. Unsupported
formats fail explicitly; stereo frames split across ring drains are preserved.
Missing callbacks trigger the three-second capture-stall deadline.

A fresh per-attempt device watcher is registered before opening the microphone.
Default-input changes cancel preparation or active capture; output-only and
general device-list notifications do not. A route change after confirmed Stop
does not discard complete captured audio while transcription drains. Callback
faults discovered during Stop still reject the result, including the shared
backend's `FormatChangedAfterStop` fault. New attempts use fresh capture,
resampler, notification and original-field state.

The shell owns real `NSWorkspace` sleep/wake observers. A notification immediately
cancels the active control flag and invalidates pending hold/review/focus work.
The shell discards stale commands/completions while preserving an explicit Quit.
Wake never replays a shortcut or starts recording. Sleep cancellation does not
release a blocked Stop or bypass native ownership.

## Stop evidence and persistent indicators

Successful Stop is distinct from complete resource release. The native owner
records Stop before waiting for callback unregistration, aggregate/process-tap
release and watcher removal. Only confirmed release makes admission available.

| Native state | Persistent indicator / next action |
| --- | --- |
| Stop remains pending | Microphone may still be recording; further preparation is blocked |
| Stop confirmed, cleanup pending | Microphone stopped — finishing cleanup |
| Stop unconfirmed | Mic may still be recording · reopen Murmur |
| Stop confirmed, cleanup failed | Mic stopped — cleanup failed; reopen Murmur |
| Cleanup confirmed | Normal ready/processing indicator resumes |

These states outlive transient notices and take priority over readiness and
permission messages. New input, prewarming and onboarding cannot bypass retained
ownership. A timeout whose cleanup later succeeds clears automatically; recording
still requires a new activation. A teardown error with the shared backend's
`Released` evidence rejects that utterance but permits a fresh attempt after the
owned objects drop. `Stopped` or `Unconfirmed` evidence retains ownership and
keeps admission blocked.

## Bounded background readiness

After a current dictation reports a microphone setup, callback, route, format,
stall or cleanup fault, the idle native shell may inspect input metadata.
Provider/configuration failures and cancelled/stale dictations do not start an
episode. The check creates no tap, IOProc or audio buffer and never records.

An episode permits at most three probes, at least fifteen seconds apart. Each
probe has a four-second handoff deadline; the next start is also spaced at least
fifteen seconds after the previous observation. A blocked metadata read retains
the same microphone-operation lease until its thread returns. Timeout,
cancellation and repeated shortcuts cannot multiply workers or start capture
beside one. Recovery waits for existing capture cleanup before probing.

Successful observations must report the same input UID and validated format
across the shell's thirty-second health window. A failure, changed route or gap
over nineteen seconds restarts health without refilling the three-attempt budget.
One success cannot heal an episode. Exhaustion leaves an explicit user-retry
notice. Metadata availability does not prove live audio, permissions or a locked
route; the ready message asks for a new dictation.

Escape, new activation, key release, sleep/wake, permission/modal work and a
frontmost-app change revoke pending readiness results. A pending OS read still
owns admission. New dictation runs fresh permission/focus checks. Readiness
completions can change only idle status, never recording control, focus,
provider selection or insertion.

## Evidence and remaining acceptance

The [capture tests](../tests/capture.rs) cover preparation/Stop/watcher deadlines,
Escape and task abort, retained ownership, stale notifications, fault/result
ordering and fresh attempts after cleanup. Split-frame preservation is covered by
`stereo_conversion_preserves_frames_split_across_ring_drains` in
[the capture module's unit tests](../src/capture.rs). The
[readiness tests](../tests/readiness.rs) exercise stable health, flapping,
observation latency, exhaustion, stale completions and blocked-worker admission.
The native sleep bridge posts notifications on an owned private Foundation center
in [its tests](../src/platform/macos_sleep.rs), without actual system sleep.
These synthetic/loopback fixtures prove policy and native bridge behavior, not
physical input, permission, Bluetooth or keyboard behavior.

The opt-in `microphone_cleanup_preview` renders the production Pill's AppKit
views in an owned nonactivating fixture, without creating Shell, input hooks,
permission requests, capture or another app's clipboard/window:

```sh
cargo run --example microphone_cleanup_preview -- /tmp/murmur-cleanup-preview
```

[Pending Stop](microphone-cleanup/pending.png),
[stopped / cleanup pending](microphone-cleanup/stopped.png),
[unconfirmed Stop](microphone-cleanup/unconfirmed.png), and
[cleanup failure](microphone-cleanup/cleanup-failed.png) are retained native
component renders. They do not establish microphone LED/callback quiescence.
Keep physical USB/Bluetooth/format-change, sleep/wake, actual next-field insertion
and signed-app permission checks unchecked until observed through the app.
