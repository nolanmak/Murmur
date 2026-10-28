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
hashes, runtime and adapter binary hashes, source commit, and environment are retained in
[m4-small-en.json](m4-small-en.json). Audio and transcripts are not committed.
The public clip contains human speech; it is not synthetic audio.

```sh
cargo build --locked --example local_transcribe
python3 scripts/qa-public-local.py \
  --model /absolute/path/to/ggml-small.en.bin \
  --wav /absolute/path/to/jfk.wav \
  --vad-model /absolute/path/to/ggml-silero-v6.2.0.bin \
  --server /absolute/path/to/whisper-server \
  --adapter /absolute/path/to/target/debug/examples/local_transcribe \
  --runtime-version 'whisper.cpp 1.9.1' \
  --output /absolute/path/to/report.json
```

The runner also verifies the pinned Silero VAD model (885,098 bytes, SHA-256
`2aa269b785eeb53a82983a20501ddf7c1d9c48e33ab63a41391ac6c9f7fb6987`; see
[runtime docs](../local-whisper-runtime.md#silero-vad-murmur-99)), because the
managed runtime refuses to start without it.

The runner imports both models into temporary owned state, starts the existing
managed gateway, performs three direct and three cloud-loss requests, stops via
the authenticated helper, and removes temporary state. If control state still
identifies an active runtime, it is retained for recovery. Existing model/audio
files and other application runtimes are untouched. Both helper and example run
under `scripts/local-whisper-offline.sb`, which denies external networking and
allows loopback. No environment/Keychain provider credential lookup is performed.

The adapter is explicitly built by the operator before the run. Its binary hash
is checked before/after trials and retained in the report. This identifies the
executed artifact; source hashes alone do not establish binary correspondence.

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
| Direct local | 0.399 s | 0.369 s | 0.365 s | All 22 words match, each run |
| Simulated cloud loss | 0.480 s | 0.484 s | 0.484 s | All 22 words match, each run |

Each recovery run observed 88,000 samples at the fake cloud before disconnect,
then accepted another 88,000 samples and returned only the local result.
The normalized reference is checked by hash, without retaining transcript text.
Request wall time includes CLI startup, readiness probe and inference. Input is
accelerated in two batches; these timings are not end-to-end live dictation
latency or measurements of a physical Wi-Fi outage.

This fresh runtime reached readiness in 0.886 s, **with uncontrolled filesystem
cache**. Model import/hash checking precedes startup. This does not establish
cold-start latency. Sampled post-readiness engine RSS peaked at 697.8 MiB and
gateway RSS at 22.5 MiB; sampling interval was 50 ms. Startup memory and separate
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

## Cancellation and capacity recovery on M4

Append `--cancellation` to the reproduction command above. After the six normal
requests, it runs three timer-cancelled adapter requests and verifies the next
request after each. The example also accepts
`local_transcribe --cancel-after-ms 50 URL WAV [LANGUAGE]` directly. The timer
starts after readiness, at the beginning of the adapter call; it is a fixture
signal, not a native Escape or focus-change event. A request completing before
observed cancellation is rejected as an invalid measurement, not called a pass.
Cancelled requests print only non-text metadata on stderr and no transcript.

The [retained report](m4-cancellation.json) records clean measured source
`c663d47de4fdabf03858c98eae130c53bd950aea`, the executed adapter/server checksums,
and the same approved public model/WAV hashes. Hardware is Mac16,10, 16 GiB,
macOS 26.4. The external-network-denied runtime used automatic GPU selection.

| Trial | Signal after adapter start | Cancellation to adapter return | Slot free after adapter process exit | Next full request |
| --- | ---: | ---: | ---: | ---: |
| 1 | 53.03 ms | 7.605 ms | 302.5 ms | 376.3 ms |
| 2 | 52.50 ms | 8.880 ms | 289.1 ms | 369.6 ms |
| 3 | 51.76 ms | 8.864 ms | 305.6 ms | 375.2 ms |

Every cancelled request returned no text. Each next request matched all 22
reference words using the same engine process; start identity was checked
before/after. All six normal direct/cloud-loss requests also matched. The runner
stopped its owned runtime and removed temporary model copies/control state.

Capacity probes send an empty POST with the bound instance/model headers. The
gateway returns 429 while its single inference slot is occupied and 413 once
free; these probes cannot start inference. There were 10–11 observed busy replies
per trial before recovery. The recovery times are sampled observations after
**adapter process exit**, with 20 ms polling plus HTTP overhead; they do not time
the exact engine completion or GPU kernel interruption. This demonstrates that
HTTP cancellation does not immediately make this managed runtime available for
another request. No engine kill/restart is used to force recovery. The measured
7.6–8.9 ms acknowledgement reflects three observations with completed inference
prioritized over cancellation, not a latency guarantee. The adapter's
cancellation polling interval remains 10 ms.

Fresh-process readiness was 0.874 s with uncontrolled filesystem cache, not a
cold-start measurement. The first normal direct request took 0.698 s; later
direct requests took 0.374–0.379 s. Post-readiness sampled RSS peaks were 697.9 MiB
for the engine and 25.8 MiB for the gateway; unique physical/GPU allocation was
not measured. No microphone, insertion, clipboard, global input, user library,
provider credential or paid request was used.

Test-first: a real loopback server returned a delayed synthetic transcript; the
new fixture initially failed with `no cancellation was measured`. It now returns
only cancellation metadata, and the server verifies the complete synthetic WAV
was sent. A second check refuses to call an already completed silence request
cancelled. CI runs these asset-free tests alongside the existing cloud-loss
fixture. This advances the cancellation/recovery evidence for #58/#60; physical
Escape, output insertion, busy-engine UX and broader release gates remain open.

Review follow-up: a forced 100 ms server-read delay reproduced the test ordering
race (`cancellation timing must start after the complete request is received`).
The synthetic test now waits for a complete-body acknowledgement before starting
its cancellation timer, and releases the late response only after cancellation
returns. Neither slow request receipt nor two elapsed timers can decide the test
outcome. The CLI still times cancellation from adapter start. The retained native
measurement above was rerun after both review fixes and identifies its exact
executed commit and binary.

A second review regression made silence completion and the signal ready in the
same poll, reproducing a false cancellation report. Biased selection now polls
inference first, so completed inference always invalidates the measurement.
This affects only the QA fixture, not production cancellation arbitration.

## Immediate next dictation after cancellation

Add `--busy-recovery` to submit the next utterance immediately after observing
busy capacity, rather than waiting for it to become free. See
[local-engine busy recovery](../local-busy-recovery.md) for policy, actual
before/after results and limits, and [the report](m4-busy-recovery.json).
`--cancellation` retains the separate capacity-release measurement described
above; the two modes can be selected independently.
