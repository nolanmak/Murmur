# Murmur

[![CI](https://github.com/nolanmak/Murmur/actions/workflows/ci.yml/badge.svg)](https://github.com/nolanmak/Murmur/actions/workflows/ci.yml)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)

**Voice dictation for your Mac. Built at Ship Systems, shared as open source.**

A macOS voice dictation app built from FlyOnTheWall's Rust audio and transcription stack.
**Hold Control, speak, release to insert. Esc cancels.** Murmur turns your speech
into text, inspired by the dictation workflow of [Wispr Flow](https://wisprflow.ai/features).
It is an independent, open-source Apache-2.0 project.

## Status

Developer build with native hold-Control dictation, Deepgram/local Whisper and
cloud-to-local fallback, bounded microphone ownership, a nonactivating indicator,
and macOS text insertion. Language selection, vocabulary hints and deliberate
spoken snippets now have native menu controls and guarded editing forms. It is
**not feature-complete or release-validated**; permissions, IME and app/device
compatibility still require the [manual QA checklist](docs/QA.md).

The [GitHub issues](https://github.com/nolanmak/Murmur/issues) contain acceptance
criteria, dependencies and test-first evidence. See [vocabulary editing](docs/vocabulary.md)
for the native forms and capability limits. [Optional text cleanup](docs/text-cleanup.md)
now has explicit provider opt-in and Offline-all controls. Real-model cleanup
quality/latency, optional history, notarized distribution and additional platforms
remain follow-on work. No claim
of full Wispr Flow parity is made.

## Build and run

Requires macOS 14.4+, Git, Xcode command-line tools, Rust installed through rustup
(the toolchain is pinned to 1.95.0), and either a configured local Whisper engine or your own Deepgram API key.
FlyOnTheWall does not need to be installed. This is a source-based developer preview;
there is no signed/notarized public release yet.

```sh
git clone https://github.com/nolanmak/Murmur.git
cd Murmur
cp .env.example .env
# Edit .env and set DEEPGRAM_API_KEY before starting the app.

cargo test --locked --workspace
cargo clippy --locked --all-targets -- -D warnings
./scripts/build-app.sh
open Murmur.app
```

The script signs with MURMUR_SIGN_IDENTITY when set, otherwise the local
`FlyOnTheWall Dev` identity when present (unlocking its dev keychain), so macOS
permissions survive rebuilds. Without either it signs ad-hoc and warns: macOS then
drops Accessibility and Input Monitoring on every rebuild. Set
MURMUR_REQUIRE_IDENTITY=1 to fail instead. The bundle is signed and verified in
`target/stage` first; a failed signature leaves the installed app unchanged.
It does not install a trust certificate or require a signing account. Start capture from the app bundle,
not a shell process, so macOS assigns permissions to this app.

1. On first launch Murmur asks for Microphone, Accessibility and Input Monitoring one
   at a time. Until all are granted, the menu status names each missing permission and
   **Set up permissions** opens the right System Settings pane.
2. After granting Input Monitoring, choose **Reopen Murmur**; macOS applies it only to
   a new launch. Dictation that could not be delivered (no Microphone, or no
   Accessibility for local insertion) is refused before recording.
3. Keep Wispr Flow on Fn. This app does not bind Fn.
   Hold **Control for 350 ms** to begin dictation; a quick tap remains a normal Control key.
   Modified Control shortcuts pass through normally.
4. Click a supported editable field, hold Control and speak, then release. Esc cancels
   during capture or processing. Pressing another key cancels dictation.
5. If insertion is blocked, use **Copy Last Transcript (replaces clipboard)**
   from the Control menu and paste manually. After a terminal or browser paste
   request, wait for the text to appear, then choose **Restore clipboard from
   local paste…**. A newer copy is kept.

## Credentials

Same provider and Keychain namespace as FlyOnTheWall, read-only. Resolution order:

1. Nonempty `DEEPGRAM_API_KEY` in the process environment.
2. The file explicitly named by `FOTW_ENV_FILE`, otherwise `.env` beside the app
   bundle, then `.env` in the working directory.
3. FlyOnTheWall's Keychain entry: service `com.flyonthewall.fotw`, account `apikey:deepgram`.

Use `.env.example` as the schema for your local `.env`. If your file is elsewhere,
specify its path with `FOTW_ENV_FILE`; contents are parsed as data, never executed.
Secrets are never bundled or committed. Finder launches can read `.env` beside
`Murmur.app`. Keychain may request permission for the app's signing identity.

Murmur retains its original internal bundle identifier, `com.shipsystems.texttospeech`,
for macOS permission continuity. Existing `TTS_SIGN_IDENTITY` configuration remains
supported; `MURMUR_SIGN_IDENTITY` takes precedence. Ad-hoc rebuilds may still require
permission renewal.

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
  Manual Copy Last Transcript changes the clipboard only when selected. Local
  insertion also uses a temporary clipboard item until explicit restore.
- Focus, field identity, secure-field status and editability are checked before local insertion.
  Local insertion uses a native paste shortcut for recognized text inputs in any app.
  Multiline/control-character output requires manual copying; no Enter key is synthesized.
- Changing apps or fields blocks automatic insertion. Cancelling invalidates late results.
- Optional Deepgram-to-local fallback is explicit; Local Whisper never falls back to cloud. LLM cleanup is separately opt-in; Offline-all selects local speech and disables cleanup. No screen context, analytics or vendor relay.
- Current microphone adapter uses the default device. Unplugging a device cancels;
  automatic device recovery is not yet implemented.
- Experimental [RustDesk review mode](docs/rustdesk.md) keeps the transcript for
  explicit review and manual clipboard paste. Automatic remote insertion and the
  macOS/Linux acceptance matrix are not yet verified.

See [architecture](docs/ARCHITECTURE.md), [provenance](docs/PROVENANCE.md),
[QA](docs/QA.md) and [contributing](CONTRIBUTING.md).

Local macOS paste behavior and recovery are documented in [local paste](docs/local-paste.md).

## Contributing and support

Bug reports, documentation improvements, and pull requests are welcome. See
[CONTRIBUTING.md](CONTRIBUTING.md) for the development workflow, or
[open an issue](https://github.com/nolanmak/Murmur/issues) with your macOS version,
reproduction steps, and expected behavior. Use synthetic text and remove private
data from logs and screenshots. See [SECURITY.md](SECURITY.md) for sensitive reports.

## License and credits

Murmur is licensed under [Apache 2.0](LICENSE). Reused FlyOnTheWall components
are credited in [NOTICE](NOTICE) and [the provenance record](docs/PROVENANCE.md).
Murmur is an independent project inspired by Wispr Flow, with no affiliation or
claim of feature parity.

### Start automatically after restart

Launch the packaged `Murmur.app` once. It enables **Open at login** by default,
so Murmur opens when you sign in after restarting macOS. The menu's **Open at
login: On/Off** item controls this setting; Off persists across manual launches.
Launching an unpackaged development executable does not register a login item.

The app maintains `~/Library/LaunchAgents/com.shipsystems.texttospeech.login.plist`
and updates its target when launched from a new location. Keep the installed app
at a permanent path, not in a temporary build/worktree directory. If setup fails,
the menu offers a retry; check write access to `~/Library/LaunchAgents` and
`~/Library/Application Support/Murmur`. Registration does not bypass macOS login
or system background-item restrictions. Quitting Murmur intentionally stops it
for the current session; the next login starts it again if the setting is On.

Native validation and restart acceptance steps: [login startup QA](docs/login-startup.md).

## Local transcription

Local Whisper supports offline dictation after explicit engine/model setup. Choose
**Transcription: Local Whisper** in the menu, or run `murmur speech local`.
See [setup, privacy and acceptance evidence](docs/local-whisper.md).

### Wi-Fi resilience

`murmur speech fallback [http://127.0.0.1:17863]` selects Deepgram with local
Whisper fallback. The Transcription menu cycles through cloud-only, local-only,
and cloud-to-local. Existing saved choices are unchanged. Install/start the
local engine first: both local and fallback modes check `/health` before opening
the microphone and never download a model while recording.

Fallback retains the complete utterance (at most 120 seconds) in memory. A
transient cloud failure gets at most one bounded retry; an unavailable cloud or
stalled finalization sends the whole utterance to the local engine after release.
Only one final transcript is returned. Auth/quota errors demote directly; invalid
requests are surfaced instead of hidden. Rate-limit waits beyond the retry budget
use local rather than retrying early. Escape and original-field validation remain
in force. This is not persistent recording/history and never switches local-only
speech to cloud. See `docs/local-whisper.md` for deadlines and test evidence.

### Vocabulary and deliberate snippets

Use `murmur vocabulary example vocabulary.json` to create an editable profile,
then `murmur vocabulary import vocabulary.json` to apply it to the next utterance.
Recognition hints are sent to Deepgram; local Whisper explicitly reports that
those hints are unavailable. Exact whole-utterance cues such as “insert snippet
sign off” expand locally with either provider. See [the versioned schema,
limits and import/export commands](docs/vocabulary.md). Native vocabulary editing is available; real-model accuracy evaluation remains in progress.

Choose **Language** in the menu bar, or save it with `murmur language es`
(English is `en`). It shares the per-utterance vocabulary snapshot and profile import/export.
Cloud support and local model capabilities are checked before recording. The
managed runtime supports languages advertised by its loaded model. Default
base.en is English-only; other languages require an explicitly imported
multilingual model. Plain external whisper.cpp servers without capabilities
remain English-only from Murmur's point of view. See [language behavior](docs/vocabulary.md#explicit-language-selection).
