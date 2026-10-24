# Bounded microphone startup

The capture coordinator adapts PR #53 (97fe25cf43cc94734d45a4b33e879e259c97e10f)
to the current local/cloud/fallback pipeline. It runs `AudioTap::start` on an
owned worker instead of the current-thread async runtime. Startup has a
four-second deadline; Escape and release before any samples arrive stop waiting.
A release after samples arrive still drains captured audio normally.

The native shell retains one `MicrophoneStartup` across attempts. If CoreAudio
stays blocked after timeout/cancellation, subsequent attempts refuse to spawn
another starter until the old one returns and its abandoned tap is stopped.
This bounds the outstanding startup worker; it cannot forcibly interrupt an OS
call. A permanently blocked OS call may require reconnecting the device or
reopening Murmur. A returned tap travels inside an RAII guard, so abandoning
the handoff also closes it. No late transcript is inserted by startup recovery.

The behavioral baseline failed three tests after the fake tap's 30-second
block: deadline, Escape and late cleanup. That checkpoint passed 11 capture tests,
including release-before-ready and repeated attempts through a shared guard.
The fixture has no physical microphone or cloud access. Native unplug/replug,
Bluetooth and sleep/wake acceptance for #61 remain required; this change does
not implement the whole device-recovery state machine or claim that OS stop
calls can be interrupted.


## Capture faults cannot become completed dictation

The callback now latches its first fault: queue overflow, device disappearance,
explicit discontinuity, non-finite samples or a generic tap error. Later
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
All 15 capture tests now pass, including those regressions, valid silent buffers
and a successful fresh attempt after a discontinuity. Fixtures use local fake
providers and taps, with no physical microphone or cloud credentials.

This is part of #61, not the full recovery implementation. Sleep-aware stall detection, bounded background readiness retries,
sustained-health reset and physical unplug/Bluetooth/sleep testing remain open.
The native tap must actually report a fault for this callback path to observe it;
missing callbacks still use the existing three-second stall timeout. No partial
transcript is automatically inserted and no background recording is started.


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
it. All 19 capture tests now pass, including input/output distinction, a fresh
attempt after a stale notification, completed-audio preservation and prompt
startup cancellation with late-tap cleanup.

The policy also recognizes explicit StreamFormat/WillSleep/DidWake events, but
the shared native watcher currently publishes only default-device/device-list
changes. Native format events and sleep-aware stall clocks,
background readiness recovery and physical device QA remain unfinished under
#61. Synthetic event tests do not establish those native acceptance gates.


## Sleep and wake cancel stale dictation

The shell now owns NSWorkspace WillSleep/DidWake observers. Each callback marks
an event pending and sets the active capture's cancel control directly, without
UI pointers or waiting for the timer. Attaching a new control while an event is
pending cancels that control too. The observer tokens remain retained and are
removed on shell teardown.

The UI checks pending sleep events before normal work and again before handling
queued completions. It invalidates the dictation generation, clears focus and
pending input/menu actions, cancels remote review and discards a pending Space
hold without replaying a space. A held shortcut cannot start recording after
wake until it is released; an explicit manual activation remains available.
No capture resumes automatically. Interrupted dictation displays a release-and-
retry message, and ordinary focus/permissions/readiness checks still apply.

Red/green tests demonstrated a pending Space hold firing after a simulated
sleep and an inactive notification callback. A boundary regression then caught
an overly broad reset blocking a fresh Space press after idle sleep; preserving
idle shortcut state fixed it. Existing controller cancellation already preserves
the held-key state, so the shell reuses it. Tests now
cover rejected old completions, a fresh activation, cancellation of current and
racing controls, both notification names and observer removal. Notification
tests use a private NSNotificationCenter; they do not post to NSWorkspace or
put the machine to sleep. Physical sleep/wake and subsequent insertion remain
native acceptance gates. These changes do not implement meeting gap recovery,
input format notifications or the remaining bounded readiness policy.


## Stereo frames across ring-buffer drains

Capture conversion now feeds interleaved samples into the stateful multichannel
resampler before downmixing its complete output frames. A drain ending after
one stereo channel is carried into the next call, rather than dropping the
unpaired sample in a stateless downmix. Final padding is sized in complete
multichannel frames; the downstream local utterance limit remains unchanged.

The regression splits a synthetic stereo waveform into odd 319-sample chunks.
Before the change it produced 7,040 output samples instead of the single-buffer
reference's 7,200. Afterward both length and waveform match. This is required
before consuming the producer's actual microphone channel-count reporting;
it does not establish physical stereo-device or Bluetooth route acceptance.
