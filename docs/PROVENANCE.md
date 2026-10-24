# Shared FlyOnTheWall components

Source: https://github.com/nolanmak/FlyOnTheWall
Pinned revision: `3acfb22b5aec7673c9169e66523b61e7391508d9`
License: Apache-2.0; root LICENSE and NOTICE retained.

Murmur resolves four crates from that immutable Git revision, with matching
normal/dev-dependency pins and Cargo.lock. There is no sibling-checkout path
requirement or moving-branch dependency:

- `fotw-audio`: capture types, microphone platform implementation and fixture seam.
- `fotw-stt`: transcript types, Deepgram wire parameters and normalization.
- `fotw-secrets`: credential namespace, redacted values and platform stores.
- `fotw-dsp`: resampling and PCM conversion, re-exported by `src/resample.rs`.

The former vendor snapshot was `5cfe8562877823fa5403a3e9cd340996d575ce33`.
Those frozen source copies are removed. The extracted DSP implementation is
byte-for-byte identical to Murmur's previous `src/resample.rs`; application
capture/deadline/cleanup and insertion policy remain in Murmur.

The native shell patterns (nonactivating AppKit panel, main-thread timer and
tray lifecycle) remain adapted from fotw-shell. The dictation controller,
focus checks and CloseStream-based transport remain application-owned.
Murmur waits for Deepgram's final Metadata after CloseStream, retaining its
existing empty-recording and endpointing behavior.

## Runtime and credential boundaries

Murmur does not enable the shared crate's `local-whisper` feature: its existing
external helper owns local inference. Importing DSP does not import meeting
storage or codecs. Shared source code does not share loaded model memory.

The producer's explicit credential stores preserve the same service/account
addresses. macOS uses its existing native Keychain store; Linux Secret Service
uses the DBus backend whose binary read/update/delete compatibility is checked
against the previous backend on an isolated bus in the producer repository.
Murmur's config precedence and local-only credential bypass are unchanged.

## Checks and upgrades

`tests/shared_credentials.rs` and `tests/shared_speech.rs` invoke producer-owned
contracts through dev-only `conformance` features, using synthetic data and no
real credentials or microphone. Murmur's capture, insertion, configuration and
transport regressions remain in this repository. The producer separately tests
its crates and an immutable migration-consumer revision on macOS and Linux.

Upgrade all eight manifest revision fields and Cargo.lock together after
reviewing the producer change. Run both repositories' required checks and the
consumer conformance suite. Roll back manifest and lockfile changes together.
A dependency version change does not imply automatic fallback, device recovery
or local inference policies have been changed.

## Native format upgrade (2026-10-24)

This revision includes merged FlyOnTheWall #137/#138/#139: model-language
validation, bounded supervisor recovery, and actual microphone format reporting.
Murmur #66 already preserves stereo frames across ring drains, so it can consume
the actual multichannel layout without discarding split frames. Unsupported
layouts fail explicitly; callback layout changes are surfaced as capture faults.
Murmur still cancels an invalid capture instead of silently inserting partial text.

The shared supervisor policy does not make Murmur use that supervisor, and the
shared Whisper adapter remains disabled: dictation still uses its managed helper.
This dependency upgrade does not establish native device/sleep acceptance or
implement the remaining Murmur device-recovery work in #61/#58.

## Device notification consumer

The producer revision now includes merged FlyOnTheWall PRs
[#140](https://github.com/nolanmak/FlyOnTheWall/pull/140),
[#141](https://github.com/nolanmak/FlyOnTheWall/pull/141) and
[#142](https://github.com/nolanmak/FlyOnTheWall/pull/142): capture-health APIs,
callback retirement, a continuous macOS capture clock and rollback of partially
registered device listeners. Murmur uses the corrected watcher registration for
its per-attempt default-input notifications. It still owns its dictation policy
and does not automatically adopt the meeting supervisor. Native input/format/
sleep notification coverage and bounded native preparation/teardown remain #61
work; changing the shared clock alone does not complete those policies.
