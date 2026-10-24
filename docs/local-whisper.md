# Local Whisper dictation

Murmur can transcribe on this Mac using a loopback whisper.cpp server. The menu's
**Transcription** item switches between Deepgram and Local Whisper. Selection is
saved to `~/Library/Application Support/Murmur/speech.json` and snapshotted when
recording starts. Switching during a recording affects the next recording.
Local mode never resolves a Deepgram key or reads Keychain credentials.

## Setup

Install a compatible `whisper-server` executable (tested with 1.9.1), then
explicitly install or import a model. See [runtime setup](local-whisper-runtime.md):

```sh
python3 scripts/local-whisper.py --app murmur setup
python3 scripts/local-whisper.py --app murmur start --binary /opt/homebrew/bin/whisper-server
python3 scripts/local-whisper.py --app murmur status
cargo run -- speech local
```

Restart an already-running app after CLI settings changes, or change the provider
in the menu. The default is `http://127.0.0.1:17863`. To use an existing server:

```sh
cargo run -- speech local http://127.0.0.1:8080
```

Only literal loopback addresses are accepted. HTTP redirects and system proxies
are disabled. This external server is never stopped by Murmur. Managed setup,
import, status, startup and shutdown use the helper; run its `--help` for options.
No model download occurs while recording. Deepgram remains the default for
existing installations; `cargo run -- speech deepgram` restores it.

## Recording and privacy

Hold Control, speak, release. Local mode captures at most 120 seconds, converts
in memory to mono 16 kHz PCM WAV, and posts `/inference` after release. It has a
separate 180-second inference timeout. At the maximum duration, the accumulator
keeps exactly the first 120 seconds; late callback samples and synthetic resampler
drain padding are clipped instead of rejecting the completed recording. Escape, focus validation and generation
checks reuse the existing delivery protections. Digital silence is skipped
before inference; this is a conservative amplitude guard, not a speech detector,
and noisy silence can still produce hallucinations with Whisper.

Murmur writes neither audio nor transcripts to history or logs. A separately
configured external server controls its own logs and temporary files. See the
runtime helper documentation for the managed engine log policy.
Cancellation closes Murmur's HTTP request and discards late output. It does not
guarantee that whisper.cpp stops GPU/CPU inference immediately; a busy engine can
continue until its current request completes. No cloud fallback is attempted.

## Acceptance evidence

Issues: #56 (provider/capture), #57 (managed runtime), #58 (QA).

- TDD red: `cargo test --test local_stt local_selection_never_resolves_cloud_credentials`
  failed with E0599, missing `Settings::capture_provider`; implementation then
  passed the same command. The local transport tests were introduced before the
  transport; initial compilation failed while that module/dependency was absent.
- Green: `cargo test --locked --workspace`; `cargo clippy --locked --all-targets -- -D warnings`;
  `cargo fmt --all -- --check`; `sh scripts/build-app.sh` (signed and verified).
- `tests/local_stt.rs`: endpoint restrictions, WAV contract, silent/pre-cancel
  no-connect behavior, persisted settings and invalid-write preservation,
  credential closure never called locally, malformed/missing text/redirect/HTTP
  errors, timeout/cancel, oversize response/audio bounds.
- `tests/capture.rs`: local release drains/resamples microphone fixture to WAV,
  closes the microphone and returns a transcript without cloud credentials.
  Existing Deepgram and delivery/generation tests remain passing.
- Runtime lifecycle tests: `python3 -m unittest discover -s scripts -p 'test_local_whisper.py'`.
- Synthetic real-engine smoke entry:
  `cargo run --example local_transcribe -- http://127.0.0.1:17863 fixture.wav`
  (mono PCM16 16 kHz WAV). This example prints synthetic fixture output for QA.

Real engine benchmark evidence is recorded separately by the integrating agent.
Native microphone, Control hold/release, focus switch, Escape, sleep/wake and
cross-application insertion remain explicit manual acceptance gates. Automated
fixtures do not establish those checks; keep their GitHub checkboxes open until
performed. Runtime cancellation and noisy-silence quality remain limitations.

Boundary TDD: `cargo test --lib maximum_capture_clips_scheduler_overshoot_and_resampler_padding`
first failed with missing `append_local_pcm` (E0425), then passed after bounded
accumulation was introduced. The test preserves the last 10 ms of permitted
speech while discarding a late callback and 200 ms resampler padding.

## Invalid settings recovery

Malformed, unreadable or non-loopback settings keep the menu-bar app open in a
blocked state. The Transcription menu identifies the error and offers an explicit
Local Whisper selection to repair settings. Recording is blocked before microphone
startup or credential resolution; settings are preserved until the user chooses
repair, and remain blocked if saving fails. There is no automatic cloud fallback.
After selecting local, the user can switch to Deepgram with another explicit menu
selection. For filesystem permission errors, fix access to the settings directory
before retrying.

Recovery TDD: `cargo test --test local_stt invalid_settings_block_capture_until_explicit_saved_choice`
failed with missing `Selection` (E0432), then passed with the recoverable state.
Tests also cover invalid endpoints, unreadable settings, failed repair, preserved
bytes before selection and a credential resolver that panics if called while
blocked. Native menu recovery remains a manual UI acceptance check.

## Explicit cloud-to-local fallback (issue #60)

Use `murmur speech fallback [URL]`, or the Transcription menu's cloud-to-local
choice. `local` remains strictly local; `deepgram` remains cloud-only. Missing
cloud credentials in fallback mode select local immediately. The local health
probe is bounded to two seconds and runs before microphone startup. Releasing
or cancelling during that probe requires a new deliberate recording.

The cloud connection has a two-second deadline. Eligible transient errors get
at most one retry, replaying the retained utterance; the minimum backoff is
100 ms, with provider Retry-After honored only when it fits the connection
budget. Finalization has a three-second overall budget after release, including
any retry. A controlled Tokio clock regression verifies that a late retry cannot
restart that budget. Local inference then has its own 180-second deadline. A 120-second
PCM bound applies regardless of connection state. Cloud partials are never
concatenated with local text. The capture generation and original-field checks
still own insertion; inference completion alone never authorizes a paste.

Tests: `cargo test --locked --test fallback --test capture --test streaming
--test local_stt`. Behavioral red cases demonstrated failure on cloud disconnect,
missing readiness preflight, invalid-request fallback, transient retry, and DNS
runtime shutdown. Focused green tests cover the corresponding corrections,
whole-utterance audio, cancellation, rate limits, capture bounds and ordinary
cloud success. The full workspace suite, Clippy, repository checks and managed
runtime tests pass; the development app builds and verifies its signature.
Native microphone/keyboard/
insertion checks remain outstanding in #58; these tests do not establish them.

Historical M4 smoke evidence recorded on the source branch (2026-10-24),
not rerun as part of this fallback extraction: isolated whisper.cpp 1.9.1 with pinned base.en
SHA-256 `a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002`,
engine and benchmark both launched under `scripts/local-whisper-offline.sb`.
The existing 20 clean synthetic 10-second fixtures measured 0.599% normalized
WER and 0.189s warm p95 raw-engine latency (first request 2.508s). Rust build work
was also running. This is synthetic speech and raw-engine timing, not human
accuracy, release-to-paste latency, a Nova-3 comparison or a completed #118 gate.
