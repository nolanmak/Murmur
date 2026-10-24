# Reused FlyOnTheWall components

Source: https://github.com/nolanmak/FlyOnTheWall
Snapshot: `5cfe8562877823fa5403a3e9cd340996d575ce33`
License: Apache-2.0; root LICENSE and NOTICE retained.

- `fotw-audio`, `fotw-stt`, `fotw-secrets` and `fotw-dsp`: maintained upstream crates
  consumed by immutable Git revision
  `32cc4590d7c19c3b39fa9e26886d1f8d906c08c0` and Cargo.lock.
  Their frozen vendor copies were removed after audio, streaming, credentials,
  config and fallback regression checks. Both apps run the shared `conformance`
  suites with synthetic audio and in-memory credentials.
  The optional in-process Whisper runtime is disabled in Murmur; its managed
  HTTP engine remains app-owned, avoiding a second linked model runtime.
- `src/resample.rs`: compatibility re-export of `fotw-dsp::resample`; the
  identical source copy was replaced by the maintained shared implementation.

Native shell patterns (nonactivating AppKit panel, main-thread timer, tray lifecycle)
are adapted from fotw-shell. The dictation controller, focus checks and
CloseStream-based transport are new. There is no runtime dependency on the original checkout.
Application policy remains in Murmur. Provider capability contracts
remain FlyOnTheWall #119 work.

Shared crate upgrades change both manifest pins and Cargo.lock together and run
the shared contract plus both apps' regression suites. Roll back the manifest
and lockfile together. Clean clones need Git access to the pinned upstream
revision, never a sibling checkout. No package publication or credential/audio
migration is involved. Shared source does not share inference memory.

The new transport waits for Deepgram's final Metadata after CloseStream. Unlike
Finalize's optional from_finalize response, this handles speech already finalized
by endpointing and empty recordings without assuming a fixed response delay.
Reference: https://developers.deepgram.com/docs/close-stream
Reference: https://developers.deepgram.com/docs/finalize


## Shared-core upgrade validation (2026-10-24)

The pin at `32cc4590d7c19c3b39fa9e26886d1f8d906c08c0` selects explicit
credential stores: the same native macOS/Windows stores and a DBus Secret
Service backend on Linux. Existing service/account addresses remain unchanged.
The producer's isolated Linux CI job 120845180526 verified reads, updates,
single-item identity and deletion against the former backend, plus refusal
when Secret Service is unavailable. No real user keychain was used. Linux
builds need system libdbus (`libdbus-1-dev` and `pkg-config`); our CI already
installs these prerequisites.

A consumer's lockfile is independent of the producer's. After moving the pin,
Murmur's advisory scan still failed on ctutils (RUSTSEC-2026-0385) and Tokio
(RUSTSEC-2026-0386). Targeted updates to ctutils 0.4.4 and Tokio 1.53.3 address
those findings. The new advisory job audits Murmur's own lockfile on pushes and
pull requests. It does not suppress advisories or rely on the producer's green
check. Preserve the manifest pin and this lockfile together when upgrading or
rolling back. Native microphone/hotkey/paste and release gates remain separate.


Validation after the lockfile updates: warnings-denied Clippy, formatting, full
locked workspace tests, repository boundaries and all 10 managed Whisper
lifecycle tests passed locally. `cargo deny check advisories` now passes.
The production graph still excludes whisper-rs, opusic-sys and ogg. A strict
`MURMUR_REQUIRE_IDENTITY=1` build succeeded with the existing dev identity;
`codesign --verify --deep --strict Murmur.app` passed. These checks do not
establish native capture or insertion acceptance. Hosted checks must run on
the new consumer revision.


The producer pin also includes native microphone ASBD validation and actual
channel counts. Murmur's conversion now resamples complete interleaved frames
before downmixing, preserving partial frames across ring-drain boundaries.
The synthetic odd-chunk regression failed at 7,040 versus 7,200 samples before
this change and matches both length and waveform afterward. Planar multichannel
and non-Float32 native input layouts are explicitly refused by the producer;
live sample-rate notification/rebuild and physical device acceptance remain open.
