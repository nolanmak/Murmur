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
block: deadline, Escape and late cleanup. All 11 capture tests now pass,
including release-before-ready and repeated attempts through a shared guard.
The fixture has no physical microphone or cloud access. Native unplug/replug,
Bluetooth and sleep/wake acceptance for #61 remain required; this change does
not implement the whole device-recovery state machine or claim that OS stop
calls can be interrupted.
