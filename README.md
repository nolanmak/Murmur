# Text-to-speech

A macOS voice dictation app built from FlyOnTheWall's Rust audio and transcription stack.
**Hold Control, speak, release to insert. Esc cancels.** Despite the project name, this is
speech-to-text, inspired by the dictation workflow of [Wispr Flow](https://wisprflow.ai/features).
It is an independent Apache-2.0 project, with its repository private during development.

## Status

Early developer build. The first vertical slice is implemented: native hold-Control input,
microphone-only capture, streaming Deepgram transcription, end-of-stream draining,
a nonactivating indicator, and insertion through macOS Accessibility. It is **not
feature-complete or release-validated**. Native permissions and app compatibility
still require the [manual QA checklist](docs/QA.md).

The [13 GitHub issues](https://github.com/nolanmak/Text-to-speech/issues) contain
acceptance criteria, dependencies and test-first work. AI cleanup, vocabulary,
snippets, optional history, signed releases, RustDesk remote paste and additional platforms are follow-on
work. No claim of full Wispr Flow parity is made.

## Build and run

Requires macOS 14.4+, Xcode command-line tools and Rust 1.95.0 (pinned).

```sh
cargo test --locked --workspace
cargo clippy --locked --all-targets -- -D warnings
./scripts/build-app.sh
open Text-to-speech.app
```

The script uses an ad-hoc developer signature by default. Set TTS_SIGN_IDENTITY
to an existing development certificate for stable signing across rebuilds.
It does not install a trust certificate or require a signing account. Start capture from the app bundle,
not a shell process, so macOS assigns permissions to this app.

1. Open the **Control** menu-bar item and choose **Set up permissions**.
2. Allow Microphone and enable this app in System Settings → Privacy & Security → Accessibility.
   If macOS requests Input Monitoring, grant it and relaunch.
3. Keep Wispr Flow on Fn. This app does not bind Fn.
   Hold **Control for 350 ms** to begin dictation; a quick tap remains a normal Control key.
   Modified Control shortcuts pass through normally.
4. Click a supported editable field, hold Control and speak, then release. Esc cancels
   during capture or processing. Pressing another key cancels dictation.
5. If insertion is blocked, use **Copy Last Transcript** from the Control menu and paste manually.

## Credentials

Same provider and Keychain namespace as FlyOnTheWall, read-only. Resolution order:

1. Nonempty `DEEPGRAM_API_KEY` in the process environment.
2. The file explicitly named by `FOTW_ENV_FILE`, otherwise `.env` in the working directory.
3. FlyOnTheWall's Keychain entry: service `com.flyonthewall.fotw`, account `apikey:deepgram`.

A `.env` was not found in the source FlyOnTheWall checkout. No unknown file or
secret was copied. The application uses the existing Keychain fallback. If your
.env is elsewhere, specify its path with `FOTW_ENV_FILE`; contents are parsed as
data, never executed. Use `.env.example` as the schema. Secrets are never bundled
or committed. Finder launches should use Keychain; their working directory is not
the repository. Keychain may request permission for the new app's signing identity.

```sh
cargo run -- doctor  # shows credential source, never the value
```

## Behavior and limits

- Microphone capture starts through Control or the Start dictation menu. No
  system-audio or screen recording. Clipboard insertion temporarily retains the
  previous clipboard in memory for restoration.
- Deepgram nova-3, mono 16 kHz PCM, smart formatting and model-improvement opt-out.
  Audio goes directly to Deepgram under your key while dictating; its API charges apply.
- 120-second session limit; bounded audio queue, capture watchdog and network deadlines.
- Audio/transcripts stay in memory. Nothing is written to a history database or log.
  Copy Last Transcript intentionally changes the clipboard only when selected.
- Focus, field identity, secure-field status and editability are checked before local insertion.
  Local insertion uses AXSelectedText or a native paste shortcut for supported fields.
  Multiline/control-character output requires manual copying; no Enter key is synthesized.
- Changing apps or fields blocks automatic insertion. Cancelling invalidates late results.
- No provider fallback, LLM cleanup, screen context, analytics or vendor relay.
- Current microphone adapter uses the default device. Unplugging a device cancels;
  automatic device recovery is not yet implemented.
- Experimental [RustDesk review mode](docs/rustdesk.md) keeps the transcript for
  explicit review and manual clipboard paste. Automatic remote insertion and the
  macOS/Linux acceptance matrix are not yet verified.

See [architecture](docs/ARCHITECTURE.md), [provenance](docs/PROVENANCE.md),
[QA](docs/QA.md) and [contributing](CONTRIBUTING.md).
