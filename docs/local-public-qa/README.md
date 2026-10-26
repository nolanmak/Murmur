# Public-fixture local transcription QA

This opt-in macOS suite exercises Murmur's actual local HTTP adapter and existing
whole-utterance fallback against an owned managed `whisper-server`. It creates
no microphone capture or text insertion. The cloud side is a loopback WebSocket
fixture that returns a partial final result, disconnects after half the input,
and closes the bounded retry. Remaining input arrives after the first disconnect.
The real local model must return the complete public reference without that
partial cloud text.

## Reproduce

Use an already-installed whisper.cpp 1.9.1 server, the existing small.en model,
and the public JFK WAV from whisper.cpp v1.9.1. The runner verifies both asset
sizes and SHA-256 values before starting anything; it downloads nothing. Their
hashes, binary hash, source commit, and environment are retained in
[m4-small-en.json](m4-small-en.json). Audio and transcripts are not committed.
The public clip contains human speech; it is not synthetic audio.

```sh
cargo build --locked --example local_transcribe
python3 scripts/qa-public-local.py \
  --model /absolute/path/to/ggml-small.en.bin \
  --wav /absolute/path/to/jfk.wav \
  --server /absolute/path/to/whisper-server \
  --adapter /absolute/path/to/target/debug/examples/local_transcribe \
  --runtime-version 'whisper.cpp 1.9.1' \
  --output /absolute/path/to/report.json
```

The runner imports the model into temporary owned state, starts the existing
managed gateway, performs three direct and three cloud-loss requests, stops via
the authenticated helper, and removes temporary state. If control state still
identifies an active runtime, it is retained for recovery. Existing model/audio
files and other application runtimes are untouched. Both helper and example run
under `scripts/local-whisper-offline.sb`, which denies external networking and
allows loopback. No environment/Keychain provider credential lookup is performed.

The direct CLI remains `local_transcribe URL WAV [LANGUAGE]`; adding
`--simulate-cloud-loss` selects the recovery path. The runner uses English for
the pinned small.en asset. The CLI snapshots the selected endpoint language
into its fallback profile; it does not implicitly switch to English.

## Retained measurement

Mac16,10, 16 GiB RAM, macOS 26.4; debug Rust example; whisper.cpp 1.9.1;
small.en; 176,000 mono 16 kHz samples (11 seconds). Helper defaults include
native automatic GPU selection; this is not a CPU-only measurement.

| Mode | Request 1 | Request 2 | Request 3 | Reference |
| --- | ---: | ---: | ---: | --- |
| Direct local | 0.712 s | 0.365 s | 0.364 s | All 22 words match, each run |
| Simulated cloud loss | 0.481 s | 0.482 s | 0.479 s | All 22 words match, each run |

Each recovery run observed 88,000 samples at the fake cloud before disconnect,
then accepted another 88,000 samples and returned only the local result.
The normalized reference is checked by hash, without retaining transcript text.
Request wall time includes CLI startup, readiness probe and inference. Input is
accelerated in two batches; these timings are not end-to-end live dictation
latency or measurements of a physical Wi-Fi outage.

This fresh runtime reached readiness in 0.643 s, **with uncontrolled filesystem
cache**. Model import/hash checking precedes startup. This does not establish
cold-start latency. Sampled post-readiness engine RSS peaked at 697.9 MiB and
gateway RSS at 25.5 MiB; sampling interval was 50 ms. Startup memory and separate
GPU allocations were not measured. RSS is not unique physical memory, and
per-process peaks need not be simultaneous.

## Test-first evidence

`cargo test --locked --example local_transcribe` initially failed the synthetic
language/whole-input regression with `Local language preflight does not match
this utterance`. Passing the selected language into the fallback profile made
it pass. The fake local server also verifies that its multipart request contains
**the exact WAV for the entire synthetic input**, including post-disconnect
samples, and the selected Spanish language. No model/audio assets are required
for this CI test. The earlier CLI invocation with `--simulate-cloud-loss` was
rejected as invalid usage before the mode existed.

Verification: formatting, strict all-target Clippy, the workspace test suite,
the example regression, 19 helper lifecycle tests, and repository boundary check
passed locally. CI runs the synthetic regression on macOS and Linux; the real
model suite remains explicit and opt-in.

## Acceptance still open

This is evidence for part of #60, not completion of #60 or #58. It does not
validate real microphone input, physical network changes, reconnect after
release, 120-second real dictation, target-app insertion, TCC, sleep/wake, a truly
cold model, simultaneous FlyOnTheWall/Murmur usage, or paired Deepgram accuracy.
A clean 11-second public clip cannot establish noisy-room, name, accent, or
meeting quality. Existing synthetic arbitration/cancellation tests complement
this measurement; they do not replace those native acceptance gates.
