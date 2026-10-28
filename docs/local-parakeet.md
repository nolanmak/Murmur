# Local Parakeet (#101)

Local Parakeet is Murmur's default local engine: Parakeet Unified 0.6B
(English, int8 Core ML) behind Silero VAD, both run by the shared
`fotw-speechd` helper from FlyOnTheWall (#243). It is the default local engine
for new installs and the engine Offline-all uses on them; Local Whisper stays
selectable and is still needed for languages other than English.

## How a dictation runs

1. **Before the mic opens**, Murmur checks that the helper is running and both
   models are installed, valid, loaded and warm (`Service::ready`, at most 2 s).
   A missing or corrupt model is refused with an install hint and nothing is
   recorded. A load still in progress (a first load compiles Core ML models and
   can take tens of seconds) is refused with "still loading".
2. That check **binds the utterance** to the helper instance and the model
   revision it saw. A load counts only for the helper instance it loaded into:
   after a helper restart Murmur loads again rather than trusting the client's
   replay, which may have failed or read a newly installed revision. If the helper restarts, or a final names another model or
   revision, the text is dropped ("no text inserted"), as with the Whisper
   gateway's instance/model headers.
3. **After release** the whole utterance (at most 120 s) goes through one
   Silero VAD stream. Unless some 256 ms step reaches a speech probability of
   0.9, nothing is typed and the recognizer never runs: Silero's 0.5 trigger
   fires for a step on some noise and instrumental music (0.54-0.87 in the
   corpus), while real speech, even a lone "Thank you.", peaks at 1.0.
   Otherwise the audio from the first speech to the last (Silero's 0.5
   regions), plus 300 ms each side, goes to one Parakeet batch stream.
4. The final text is joined into **one line** (#98), without Parakeet's
   `<unk>` markers, and then goes through the
   same command, snippet, cleanup, focus and generation checks as every engine.

Murmur spawns and owns the helper (`fotw-speechd-client`: restarts with
backoff, a crash fails the open stream within a second). It loads and warms
both models at launch and whenever a Parakeet choice is made, so the first
dictation pays no load or compile. The helper never opens the microphone and
carries no entitlements; Murmur sends it PCM over a pipe.

## Choosing it

| Where | How |
|---|---|
| Menu | Transcription → Local Parakeet, or Deepgram with Parakeet backup |
| Voice | "Murmur, use Parakeet"; "Murmur, use local" when Parakeet is the default local engine |
| CLI | `murmur speech parakeet`, `murmur speech fallback parakeet` |

Settings (`~/Library/Application Support/Murmur/speech.json`) have a
`local_engine` field: the engine "use local", "use backup" and Offline-all
mean. New installs get `parakeet`. A file written before this field existed
keeps `whisper`, and so does a machine with no `speech.json` that has the
managed Whisper runtime installed (`~/Library/Application Support/murmur/local-whisper/model.bin`),
so nobody is moved off Whisper until they choose Parakeet; naming either
engine makes it the default.

## Installing the model

The only paths that download are explicit:

- **Transcription → Install Parakeet model…** (only while no dictation runs;
  dictation is refused until it finishes), or
- `murmur speech install-parakeet [--parakeet-from DIR] [--vad-from DIR]`; the
  `--from` options import a local copy and open no socket.

Models live in `~/Library/Application Support/Murmur/models`
(`MURMUR_SPEECHD_MODELS` overrides it), each pinned to a revision with every
file's size and SHA-256 checked by the helper. A failed install keeps the
previous model. After setup Local Parakeet works with the network denied.

Licenses: Parakeet Unified is CC-BY-4.0 (NVIDIA), Silero VAD MIT, FluidAudio
Apache-2.0. They are shown under Transcription → Local model licenses and in
`NOTICE` / `THIRD_PARTY_NOTICES.md`, which the app bundle includes.

## Building the helper into Murmur.app

```sh
export FOTW_FLUIDAUDIO_SOURCE=/path/to/FluidAudio   # 0.17.3 at a6b826ce…
MURMUR_SPEECHD=1 ./scripts/build-app.sh
```

`scripts/build-speechd.sh` builds `fotw-speechd` from the FlyOnTheWall source
Cargo.lock pins (Swift 6.2+), and `scripts/sign-app.sh` signs it into
`Contents/Helpers/fotw-speechd` before the app, with its own identifier,
the hardened runtime and `packaging/speechd-entitlements.plist` (empty). The
build fails if the helper is missing, carries the microphone entitlement, or
is not hardened. Without `MURMUR_SPEECHD=1` no Swift is needed and Local
Parakeet reports that the build lacks the helper.

The pinned revision contains the helper (FlyOnTheWall #253, bundled as in
#254). `MURMUR_SPEECHD_SOURCE=<FlyOnTheWall checkout>` builds from another
checkout instead, for local work on the helper only.

## Tests

- Ordinary suite (offline, no model): `tests/local_parakeet.rs` uses a fake
  loader for the refusal, binding, gating, trimming and one-line rules;
  `tests/speechd_backend.rs` runs the real adapter against FlyOnTheWall's
  model-free `fotw-speechd-fake`, built offline from the pinned source.
- Real-model suite (separate, explicit):
  `python3 scripts/eval-local-parakeet.py --helper PATH --models DIR --corpus DIR`
  runs `examples/parakeet_eval` with the network denied over the shared
  corpus (FlyOnTheWall #247 corpus-v2) and gates latency, WER and non-speech.
  `--short-set DIR` (built by `scripts/make-short-speech-set.py`: one-word
  TTS and LibriSpeech utterances, the thank-you and speech clips, each at 0,
  -12 and -20 dB) adds recall for short and quiet speech, and gates that every
  such clip passes the speech gate (`scripts/speechd-vad-peaks.py`).
