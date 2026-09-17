# Reused FlyOnTheWall components

Source: https://github.com/nolanmak/FlyOnTheWall
Snapshot: `5cfe8562877823fa5403a3e9cd340996d575ce33`
License: Apache-2.0; root LICENSE and NOTICE retained.

- `vendor/fotw-audio`: microphone capture, native permissions and fixture seam.
- `vendor/fotw-stt`: Deepgram wire parameters, streaming normalization and transport regression tests.
- `vendor/fotw-secrets`: redacted secret values and the existing OS Keychain namespace.
- `src/resample.rs`: copied from `crates/fotw-pipeline/src/resample.rs`.

Native shell patterns (nonactivating AppKit panel, main-thread timer, tray lifecycle)
are adapted from fotw-shell. The dictation controller, focus checks and
CloseStream-based transport are new. There is no runtime dependency on the original checkout.
The vendored sources are unchanged; new behavior belongs in the application layer.

The new transport waits for Deepgram's final Metadata after CloseStream. Unlike
Finalize's optional from_finalize response, this handles speech already finalized
by endpointing and empty recordings without assuming a fixed response delay.
Reference: https://developers.deepgram.com/docs/close-stream
Reference: https://developers.deepgram.com/docs/finalize
