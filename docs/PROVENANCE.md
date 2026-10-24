# Shared FlyOnTheWall components

Source: https://github.com/nolanmak/FlyOnTheWall
Pinned revision: `19d6b4585bbd2f98f207dce6732493cc42ad91d9`
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
