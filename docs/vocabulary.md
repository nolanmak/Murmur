# Personal vocabulary and snippets (#9)

Murmur reads `~/Library/Application Support/Murmur/vocabulary.json` once before
each utterance, before credentials or microphone startup. Changes apply to the
next utterance. Missing files mean an empty profile; malformed files refuse the
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
counts and capability limitations without printing profile contents.

```json
{
  "version": 1,
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

This is partial delivery of #9. Explicit persisted language selection and
runtime/model language validation remain open. Existing provider defaults are
unchanged; the profile deliberately has no language setting to silently ignore.
Real-model term-recall comparisons (#118), native dictation/insertion checks and
full vocabulary editing UI remain unverified. Do not claim recognition-quality
improvement from transport tests. Optional AI cleanup (#8) is separate and has
not been enabled by this change.
