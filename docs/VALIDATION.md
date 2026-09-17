# Validation record

## Test-driven development

- Fn controller: three behavioral tests failed against the initial no-op controller;
  hold/release, repeated events and Escape now pass, with stale-generation and chord regressions added.
- Configuration: process-key and dotenv/keychain cases failed against the initial resolver;
  precedence, literal parsing, missing files and secret-free diagnostics now pass.
- Provider stream: the trailing-final integration test failed before transport implementation;
  real localhost WebSocket tests now cover final speech, premature disconnect,
  empty/interim-only input, completion timeout and rejecting remote plaintext endpoints.
- Capture: a release-to-provider fixture test was added before its injectable endpoint API;
  the final integration drives a fake mic through the real resampler and socket adapter.
- FlyOnTheWall's existing anti-aliasing and broader vendored regression tests are retained.

## Checks

- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets -- -D warnings`
- `cargo test --locked --workspace`
- `python3 scripts/check-repo.py`
- `scripts/build-app.sh`: developer .app built and ad-hoc signature verified.
- Native app launched; first-run indicator text inspected through macOS Accessibility
  and a screenshot. The app was restarted after the user reported accepting permissions.
- Existing FlyOnTheWall Deepgram Keychain entry presence confirmed without reading its value.

Real microphone transcription, Fn event delivery and editor compatibility remain
user smoke tests; automated success does not establish those native behaviors.
No live provider request was made by the automated suite.

## Revised shortcut: hold Control

Five Control tests failed first, then passed: quick taps, 350 ms hold, duplicate
repeats, modified shortcuts, typing rollover and Escape cancellation. Fn is no
longer registered by the native adapter. The rebuilt test app uses an ad-hoc signature. Stable certificate signing remains
a release task; TTS_SIGN_IDENTITY supports an existing development identity.
