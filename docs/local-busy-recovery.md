# Local-engine busy recovery

A cancelled HTTP request can stop delivering output before the native engine
finishes computing. A new short dictation can therefore encounter a busy local
engine. Murmur now retains that attempt's bounded PCM and retries explicit HTTP
429 refusals without asking the user to speak again during a brief busy period.
This applies to Local-only and the local phase of Deepgram→local fallback.

## Behavior

- At most five retries after the first request, with a minimum 100 ms delay.
  Further retries are declined if their scheduled start would be one second or
  more after the first busy response. All attempts and waits share the caller's
  original local-inference deadline; it is never reset per retry.
- Retry-After is never shortened. A delay outside this short retry window,
  unsupported date syntax, invalid value or duplicate header produces the busy
  failure instead of an early retry. A zero delay still waits 100 ms.
- Each retry sends the full original WAV, selected language and preflight
  instance/model binding. No partial busy-response text is accepted. A changed
  runtime/model cannot be adopted implicitly.
- Cancellation remains checked during waits and before another request. Other
  HTTP or transport failures are not retried as busy: an uncertain inference
  completion must not cause duplicate engine work.
- Persistent busy failure says to wait for the current transcription and try
  again; no text is inserted. Ended/cancelled attempts release their buffers.
  This adds no audio/history persistence, engine restart, downloads, credential
  resolution or cloud promotion.

The one-second window governs retry admission, not the duration of an admitted
inference. A model may legitimately take longer; the overall inference deadline
still applies. Cancellation does not promise immediate compute interruption.

## Regression-first evidence

Before implementation, four of five initial loopback checks failed with the
generic rejected/not-ready error. After implementation, all seven transport
checks pass: complete input/language/binding, persistent busy, Retry-After,
one overall deadline, other status errors, cancellation without a new request,
and refusal to adopt a replacement model. Two additional fake-clock/header
policy tests cover independent count/time bounds, overflow and malformed hints.

## Real approved-fixture M4 check

The opt-in command extends the [public-fixture runner](local-public-qa/README.md):
append `--busy-recovery` to its reproduction command. After six direct/cloud-loss
checks, it cancels a real adapter request, observes busy capacity, and immediately
submits the next public clip without explicitly polling/waiting for a free slot.

The previous adapter (binary SHA-256
`dcf524eb7a8f8589ea5f8ab781abac79f497582baa4e0e5b946d081a416ddff2`,
measured source c663d47) rejected that immediate next request. Its owned runtime
shutdown and temporary-state removal were verified even on this failing run.
The fixed adapter passed all three immediate-next-request trials:

| Trial | Full next-request wall time | Reference |
| --- | ---: | --- |
| 1 | 0.708 s | All 22 words match |
| 2 | 0.710 s | All 22 words match |
| 3 | 0.704 s | All 22 words match |

[Retained report](local-public-qa/m4-busy-recovery.json): clean measured source
`e8306a0641086616cfb54ebf3e7c318a320bcf63`, actual executed adapter/server hashes,
and pinned model/audio checksums. Mac16,10 / 16 GiB / macOS 26.4, whisper.cpp
1.9.1 with default automatic GPU selection; no external networking for the
runtime or adapter. Only the already approved small.en model and public JFK WAV
were used; no model/audio was downloaded or committed.

Every trial observed 429 immediately before dispatch and 413 after the complete
next request. Empty bound capacity probes never invoke inference. Native HTTP
retry counts were not instrumented; the synthetic transport tests establish
those counts. These wall times include CLI startup, health and inference, and
do not separately time the original request's slot release or GPU work. The six
normal direct/cloud-loss requests also matched the complete reference. Engine
start identity was unchanged. The owned runtime and temporary model/control
state were removed after both before/after runs.

This is adapter acceptance, not a signed-app microphone, physical Escape,
focus/insertion, USB/Bluetooth or sleep/wake result. Those laptop gates remain
open in #58/#60. Existing provider choices, cleanup and capture ownership remain
unchanged.
