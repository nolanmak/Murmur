# Reused FlyOnTheWall components

Source: https://github.com/nolanmak/FlyOnTheWall
Snapshot: `5cfe8562877823fa5403a3e9cd340996d575ce33`
License: Apache-2.0; root LICENSE and NOTICE retained.

- `fotw-audio`, `fotw-stt`, `fotw-secrets` and `fotw-dsp`: maintained upstream crates
  consumed by immutable Git revision
  `3781146810541f49f44a7f86ae79cea4b2514e5e` and Cargo.lock.
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
