# Bounded microphone startup

The capture coordinator adapts PR #53 (97fe25cf43cc94734d45a4b33e879e259c97e10f)
to the current local/cloud pipeline. It runs `AudioTap::start` on an
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

Ten capture tests pass, including the deadline, Escape, late cleanup,
release-before-ready and repeated attempts through a shared coordinator.
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

This is part of #61, not the full recovery implementation. Native route-change
notifications, sleep-aware stall detection, bounded background readiness retries,
sustained-health reset and physical unplug/Bluetooth/sleep testing remain open.
The native tap must actually report a fault for this callback path to observe it;
missing callbacks still use the existing three-second stall timeout. No partial
transcript is automatically inserted and no background recording is started.
