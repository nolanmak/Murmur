# Personal vocabulary, language and snippets (#9)

Murmur reads `~/Library/Application Support/Murmur/vocabulary.json` once before
each utterance, before credentials or microphone startup. Changes apply to the
next utterance. Missing files mean an empty English profile; malformed files refuse the
attempt instead of silently losing settings. The native completion/generation,
focus and cancellation checks still control insertion.

Generate and validate a profile, edit it, then import it explicitly:

```sh
murmur vocabulary example vocabulary.json
murmur vocabulary validate vocabulary.json
murmur vocabulary import vocabulary.json
murmur vocabulary export vocabulary-backup.json
```

These commands do not capture audio. Example/export write the named file; import
replaces the app's profile atomically only after complete validation. The schema
has no credential/provider-endpoint fields. It can contain personal vocabulary
and snippet text, so handle exported profiles accordingly. `doctor` reports
counts, the selected language and capability limitations without printing hints or snippets.

```json
{
  "version": 1,
  "language": "en",
  "recognition_hints": ["Project Acorn", "C++", "Zoë & Sons"],
  "snippets": [
    {"name": "sign off", "text": "Thanks,\nExample team"}
  ]
}
```

Recognition hints become repeated, percent-encoded Nova-3 `keyterm` parameters.
They are recognition hints, not deterministic text substitutions. Cloud retries
use the same immutable list. Snippet bodies never enter recognition prompts.
Deepgram documents a [500-token keyterm limit](https://developers.deepgram.com/docs/keyterm).
Murmur imposes a conservative separate limit of 500 UTF-8 bytes including one
separator per hint, at most 64 hints, and 128 bytes per hint; it does not claim
to reproduce Deepgram's tokenizer. Empty/untrimmed/control-containing or
case-insensitive duplicate hints are refused.

Local Whisper does not implement Nova-3 keyterms. It receives no hints and never
contacts a hosted model to repair them. The native progress label and successful
completion message explicitly report this difference when hints are configured.
Deterministic snippets still apply to the winning local transcript.

Say a complete utterance such as **"insert snippet sign off"** to expand a
snippet. Matching ignores case and surrounding whitespace and allows one final
`.`, `!` or `?`. It does not expand a cue embedded in a sentence, repeated cues,
unknown names, plurals, or quoted code. There are no global word replacements.
Names contain letters/numbers, single spaces, hyphens or underscores, are at
most 80 UTF-8 bytes, and must be unique ignoring case. Snippet text is preserved
verbatim, including newlines/tabs; other control characters are refused. Limits:
128 snippets, 16 KiB per body and 256 KiB per serialized profile. Unsupported
versions, unknown fields, conflicting names and oversized inputs are rejected
before replacing existing settings.

## Evidence and remaining scope

The first behavioral tests failed because the scaffold left cues unchanged,
accepted unsupported versions and overwrote prior settings on invalid save.
Tests now cover exact boundaries, accented Unicode, overlapping names, prose and
code preservation, size limits, atomic failure, CLI example/validation, request
encoding, identical hints across cloud retry, and local capability reporting.
A real capture-path fixture changes the on-disk profile during transcription and
verifies that only the winning final is expanded with the original snapshot.
An invalid profile never starts the fixture microphone.

Run the focused tests, plus the formatting, Clippy and workspace checks in
CONTRIBUTING.md:

```sh
cargo test --locked --test vocabulary --test streaming --test fallback --test capture
```

All test audio/text and credentials are synthetic.

This is partial delivery of #9. Explicit language selection and client-side
preflight/binding and the managed language adapter are implemented below.
Language/vocabulary editing UI and real speech acceptance remain open.
Real-model term-recall comparisons (#118), native dictation/insertion checks and
full vocabulary editing UI remain unverified. Do not claim recognition-quality
improvement from transport tests. Optional AI cleanup (#8) is separate and has
not been enabled by this change.

## Explicit language selection

`murmur language es` persists Spanish in the same version-1 profile. It applies
to the next utterance without restarting the app. Language also survives profile
import/export. Older profiles without the field default to `en`. Use canonical
codes such as `en`, `es`, `fr-CA`; malformed tags fail without replacing settings.
Automatic detection and code-switching are not implied by this setting.

Before starting the microphone, cloud modes check the selected code against the
[Nova-3 monolingual language table](https://developers.deepgram.com/docs/models-languages-overview#nova-3)
(verified 2026-10-24). Both cloud attempts receive the same explicit `language`
parameter and recognition hints from the utterance snapshot. No alternate cloud
model or language is silently substituted.

Local modes check the engine's language/model capabilities before recording.
The managed helper reports the loaded model's languages and enforces the
[capability contract](local-language-contract.md). Its default base.en model
supports English only; explicitly import a multilingual GGML model while stopped
and restart the helper before selecting another supported language. No model is
downloaded during capture. Plain external whisper.cpp servers report readiness
only, so non-English requests to those endpoints are refused before capture,
even if the filename suggests a multilingual model.

For a capability-aware adapter, the prepared endpoint carries the selected
language, model SHA and serving instance ID through capture and fallback.
Inference sends explicit language, translation=false and detection=false.
Missing/different response identities refuse insertion. Local recognition hints
remain unsupported; they are omitted with the existing visible notice.

Language RED/GREEN: the capture fixture selected Spanish but the actual cloud
handshake omitted language; it now sends Spanish while an on-disk edit to French
only affects the next utterance. Tests cover language/profile roundtrip, legacy
English defaults, invalid-write preservation, identical French on both cloud
attempts and local fallback, and refusal before microphone start for unknown
cloud codes and unverifiable local/fallback language. Loopback fixtures verify
bounded capability parsing, unsupported/malformed capabilities, explicit local
wire settings, and rejection of changed/missing runtime/model identities. These
are transport/lifecycle tests. The managed adapter additionally passed real-engine
synthetic English/French smoke checks; human multilingual accuracy remains a
separate evaluation.
