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

The earlier Space tests were renamed for Control and passed; the Control changes
were not developed test-first and those tests did not exercise native event delivery. Fn is no
longer registered by the native adapter. The rebuilt test app uses an ad-hoc signature. Stable certificate signing remains
a release task; TTS_SIGN_IDENTITY supports an existing development identity.

## Live diagnosis, September 17

- macOS TCC logs explicitly reported a code-requirement mismatch for Input Monitoring
  after ad-hoc rebuilds. Enabled Settings switches did not establish access for the current binary.
- The live manual smoke entry (`run --smoke`) reached the microphone permission guard;
  transcription could not start because this app had no microphone grant.
- Added Start/Stop dictation menu actions, animated listening status, permission diagnostics,
  and a listen-only keyboard tap. Unsupported editors can record for manual copying.
- Added the manual restart regression before `start_manual`: compilation failed on the
  missing API, then passed after implementation. Existing mock transport tests do not prove
  real OS permissions or live provider connectivity.
- After microphone authorization, the manual smoke run showed the Listening overlay;
  two UI observations captured different animation frames. The worker then blocked in
  `config::load` → `OsKeyStore::get` → macOS `SecKeychainFindGenericPassword`.
  The key was not retrieved and no live Deepgram transcription was verified. The app
  was restarted without smoke mode to stop the pending Keychain request.
- Keyboard permission refresh reached a macOS Touch ID/password authorization sheet.
  Keyboard delivery and automatic text insertion remain unverified pending that grant.
- The current Listening animation indicates the requested session state; it does not yet
  prove the microphone has opened while credential loading is pending. Treat this as a
  known UX limitation, not a passed end-to-end capture test.
