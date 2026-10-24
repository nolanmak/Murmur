# Local Whisper QA — 2026-10-15

This evidence covers the new local STT adapters and managed runtime. No personal
recordings or native agent sessions were used. Detailed synthetic references,
transcripts, timings and limitations are in [the JSON report](local-whisper-qa.json).

## Environment and results

- Apple M3, 24 GiB unified memory; whisper.cpp 1.9.1 with Metal; pinned base.en
  SHA-256 `a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002`.
- Both server and test clients ran under the included macOS sandbox profile,
  which denies external networking and permits loopback. External hostname and
  numeric-IP probes failed; local inference succeeded.
- Twenty synthetic clean-English utterances, each padded to exactly 10 seconds:
  normalized WER **0.60%**; raw-engine warm p95 **0.138 seconds** across 19 calls
  after the first request. These meet the proposed 10%/3-second gates for this
  narrow corpus only. Audio generation/conversion is outside raw inference timing.
- Technical fixture: one error, “Rust” → “REST”. This is not a guarantee of
  human microphone, accent, vocabulary, or noisy-room accuracy.
- Murmur's compiled adapter transcribed clean WAV and varied 120-second speech;
  digital silence returned empty. Its capture tests separately verify draining,
  resampling and completion through the local adapter.
- Agentflow's compiled adapter transcribed WAV, WebM, MP4 and noisy WAV locally;
  silence returned empty. Observed short-input adapter times were 0.128–0.407s,
  including local format conversion. After the direct-HTTP transport hardening,
  repeated speech checks passed in 0.124–0.161s and silence in 0.027s.
  A 90-second fixture transcribed in 1.099s;
  120 seconds failed with the documented 90-second limit.
- Two independently managed engines ran simultaneously. Repeated start was
  idempotent. Stopping either preserved the other; offline restart passed.
  State directories contained spaces. Startup ranged 0.335–7.837s in these runs.
- Server RSS snapshot: 311,136 KiB. This is not a peak or full Metal memory metric.

## Reproduce the raw-engine benchmark

Install runtime/model explicitly using the runtime guide first. Start an engine
with networking restricted (use a free port and your model path):

```sh
sandbox-exec -f scripts/local-whisper-offline.sb whisper-server \
  --host 127.0.0.1 --port 17865 -m /absolute/path/to/model.bin
```

In another terminal:

```sh
sandbox-exec -f scripts/local-whisper-offline.sb python3 \
  scripts/benchmark-local-whisper.py --url http://127.0.0.1:17865 \
  --output /tmp/local-whisper-benchmark.json
```

The script uses macOS `say` and `ffmpeg`, reads committed synthetic reference
text, generates temporary audio, and deletes that audio when it finishes. It
never downloads a model or submits anything to a native agent. This benchmark
tests the raw engine; the app-specific adapter tests remain necessary.

## Remaining gates and limits

Physical microphone/permission checks, real Control hold/release and focused
app insertion, sleep/wake, and browser hardware acceptance have **not** been
performed. Keep those QA issue checkboxes open. Automated capture, browser and
session fixtures do not prove physical-device behavior.

Raw Whisper produced “you” on digital silence. Both adapters prevent this for
their silence thresholds; noisy silence can still hallucinate. Aborting an HTTP
request prevents insertion/submission but does not promise immediate engine
compute interruption. Local STT does not make separately configured TTS or
native Codex/Claude reasoning local.

## Loopback startup and hostname lookup

The supervisor now binds its literal loopback address without Python
HTTPServer's `socket.getfqdn` call. Local readiness must not depend on reverse
DNS availability. The synthetic HTTP fixtures also avoid that lookup. A test
that makes `socket.getfqdn` fail reproduced the dependency before the change;
all 10 managed-runtime tests pass after it. This removes a plausible cause of
the hosted macOS readiness failure, but a successful hosted rerun is still
required before calling that CI failure resolved.
