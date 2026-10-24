# Reused FlyOnTheWall components

Source: https://github.com/nolanmak/FlyOnTheWall
Snapshot: `5cfe8562877823fa5403a3e9cd340996d575ce33`
License: Apache-2.0; root LICENSE and NOTICE retained.

- `vendor/fotw-audio`: microphone capture, native permissions and fixture seam.
- `vendor/fotw-stt`: Deepgram wire parameters, streaming normalization and transport regression tests.
- `fotw-secrets`: maintained upstream crate consumed by immutable Git revision
  `996fd79c39f7fe19eb14e3ace68c3a493540d12a` and Cargo.lock; redacted values and the
  existing OS Keychain namespace. The unchanged vendor copy was removed after
  config/local/fallback parity tests. Both apps run the shared `conformance` suite.
- `src/resample.rs`: copied from `crates/fotw-pipeline/src/resample.rs`.

Native shell patterns (nonactivating AppKit panel, main-thread timer, tray lifecycle)
are adapted from fotw-shell. The dictation controller, focus checks and
CloseStream-based transport are new. There is no runtime dependency on the original checkout.
The remaining vendored audio/STT sources are unchanged; new app policy belongs
in the application layer. Their migration remains FlyOnTheWall #119 work.

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
