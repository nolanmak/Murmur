# Local Whisper development runtime

Requires Python 3.9+ and a separately installed `whisper-server`. Tested with whisper.cpp **1.9.1** on Apple Silicon. Install deliberately with `brew install whisper-cpp` (Homebrew may supply a newer version), or build upstream v1.9.1 and pass `start --binary '/path with spaces/whisper-server'`. The helper never installs executables or implicitly downloads models. No Python packages are required.

From the repository root, substituting `agentflow` for `murmur` where appropriate:

```sh
python3 scripts/local-whisper.py --app murmur setup
python3 scripts/local-whisper.py --app murmur start
python3 scripts/local-whisper.py --app murmur status
python3 scripts/local-whisper.py --app murmur stop
```

`setup` explicitly downloads base.en (~148 MB) and the Silero VAD model (~0.9 MB), verifies their SHA-256s, then atomically selects them. `setup --vad-only` installs just the VAD model and keeps the selected speech model. Alternatively, import an existing model:

```sh
python3 scripts/local-whisper.py --app murmur import-model '/path/to/ggml-base.en.bin' --sha256 a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002
```

Custom models require their known SHA-256. Runtime start rechecks integrity offline. Corrupt or interrupted imports preserve the previous model selection; verified immutable digest-named files are selected through an atomic symlink replacement. Old verified models remain on disk for manual removal while stopped. Stop before changing the selected model. State defaults to `~/Library/Application Support/<app>/local-whisper`; override via `--state-dir PATH` before the subcommand. State is private to the current user.

Murmur defaults to `http://127.0.0.1:17863`; Agentflow uses `http://127.0.0.1:17864`. `start --port N --timeout 30` overrides the port and readiness deadline (maximum 300 seconds). Configure the app's local transcription endpoint to match. Start is idempotent for an already-owned runtime, retaining its original configuration; stop before changing binary or port.

A private authenticated loopback supervisor owns the child process. Its random bearer token is handed over through a private stdin pipe with a five-second deadline and strict frame-size/format validation; it never appears in process arguments or public CLI JSON. Stop contacts this supervisor and terminates only its own child, never a PID from a stale state file. Runtime output is discarded to avoid storing transcripts in logs. Readiness and control ignore HTTP proxy variables and reject redirects. A taken port reports an error rather than adopting or stopping another server. To use an external server, configure its loopback endpoint in the app and manage that process yourself; helper stop will not affect it. These are development CLI controls, not a bundled production installer or automatic app-exit hook. App and helper lifecycle remain explicit.

Setup requires internet. Start, status, stop and transcription can run offline after model installation. Local transcription does not make unrelated cloud LLM/TTS features local.

Pinned model source: [ggerganov/whisper.cpp revision 5359861c739e955e79d9a303bcbc70fb988958b1](https://huggingface.co/ggerganov/whisper.cpp/tree/5359861c739e955e79d9a303bcbc70fb988958b1), file `ggml-base.en.bin`. SHA and size (147,964,211 bytes) verified from the [Hugging Face model API](https://huggingface.co/api/models/ggerganov/whisper.cpp/revision/5359861c739e955e79d9a303bcbc70fb988958b1?blobs=true). Runtime source: [whisper.cpp v1.9.1](https://github.com/ggml-org/whisper.cpp/tree/v1.9.1).

Tests: `python3 -m unittest discover -s scripts -p 'test_local_whisper.py'` (19 passed at the time; 24 with the VAD tests). Initial four acceptance tests failed before implementation; the current suite passes. Coverage includes verified imports, corrupt/interrupted replacement preservation, corrupted model startup, missing model, authenticated idempotent lifecycle, paths with spaces, external port collision, stale records, exit-zero/unready engines, readiness timeout cleanup, rejected redirects, and bounded secret handoff. The lifecycle test reads the actual process list and verifies that the secret is absent while authenticated status and stop still work. Real installed 1.9.1/base.en import → start → ready → stop was also exercised. Audio accuracy and end-to-end app insertion/submission require separate app QA.

CI timing: the successful-engine fixture allows a 30-second readiness deadline and 60 seconds for each CLI subprocess, accommodating cold macOS runner startup. The deliberate unready-engine case retains its 0.5-second readiness deadline; readiness, process ownership, port collision, and timeout cleanup assertions are unchanged. The process-list token assertion failed before the private-pipe fix and passed afterward.

## Model language metadata (Murmur #9 groundwork)

Import and startup validate the fixed 48-byte Whisper GGML header after checksum
verification. Unsupported magic, truncated headers, unknown vocabulary layouts
and inconsistent dimensions fail before replacing the selected model or starting
the engine. Status reports `unsupported_model` for a checksum-valid legacy file
with an unsupported header. Header validation does not validate tensors: engine
loading and readiness remain required.

`status.selected_model_metadata` reports the selected file's SHA-256, format,
vocabulary size, multilingual flag and language count. The supported layouts
are 51,864 tokens (English only), 51,865 (99 languages), and 51,866 (100 languages),
following [whisper.cpp v1.9.1 model loading and vocabulary metadata](https://github.com/ggml-org/whisper.cpp/blob/v1.9.1/src/whisper.cpp).
This is file metadata, **not an attestation of the model loaded by the HTTP
server**. It must not authorize a language choice for an arbitrary endpoint.
The upstream server health response has no language capabilities and its load
route can replace the running model. The managed gateway now enforces a [language capability contract](local-language-contract.md)
against the owned, loaded model and advertises that separate live binding through
health/status. The client refuses unverifiable non-English selections from plain
external servers. Selected-file metadata must not be substituted for live health.

Behavioral RED: the checksum-valid non-model import test returned success and
replaced the selection; status lacked model metadata. GREEN: 13 lifecycle tests
pass, including preserved selection and temporary-file cleanup for malformed
headers, English/multilingual metadata despite misleading filenames, and legacy
invalid-model refusal before the fake engine is launched. Existing lifecycle,
secret-handoff, redirect and integrity tests remain passing. The parser also
read the installed small.en header as English-only; no inference or microphone
was used for that check. Synthetic headers in lifecycle fixtures do not prove
that real model tensors load.

## Managed inference gateway

The selected public port now serves the capability-aware gateway; whisper.cpp
runs behind it on an internal loopback port/path. Keep both `local-whisper.py`
and `whisper_gateway.py` together. Requests must preflight `/health` and send the
returned instance/model headers; current Murmur does this automatically. The
synthetic `local_transcribe` example also preflights and accepts an optional
language argument. Direct curl/older clients must follow the documented contract.
The managed port rejects model loading and unsupported parameters. Restarting
creates a new instance binding, so an utterance checked against an older runtime
cannot be silently submitted to its replacement.

The pinned base.en setup remains English-only. To use another language, stop,
explicitly import a compatible multilingual model with its known checksum,
restart, and run `murmur language fr` (or another advertised code). Inference
stays local. The language table is tied to the tested whisper.cpp 1.9.1 runtime;
compatibility with unrelated/custom runtime implementations is not guaranteed.

Behavioral RED: the managed-runtime HTTP fixture returned readiness but no
capabilities. GREEN: 19 lifecycle tests pass, including successful bound French
inference, 99-language metadata, English-only rejection, stale instance/model
refusal, restart changes, public `/load` refusal, invalid/duplicate fields,
oversized requests, concurrent inference refusal, Stop during blocked inference,
and retirement after timed-out/oversized/malformed backend responses. Existing
checksum/import, ownership, private-token, redirect and startup cleanup tests
remain passing. Real-engine English/French smoke evidence and remaining native
acceptance are recorded in the capability contract document.

## Silero VAD (Murmur #99)

The managed engine starts `whisper-server --vad --vad-model <verified file>`.
whisper.cpp runs Silero VAD over each request and only transcribes detected
speech; a request with no speech returns `{"text": ""}`, which Murmur reports as
"No speech detected" without inserting anything. The gateway advertises
`"vad": true` in `murmur_capabilities`.

The VAD model is pinned: `ggml-silero-v6.2.0.bin` from
[ggml-org/whisper-vad revision 9ffd54a1e1ee413ddf265af9913beaf518d1639b](https://huggingface.co/ggml-org/whisper-vad/tree/9ffd54a1e1ee413ddf265af9913beaf518d1639b),
885,098 bytes, SHA-256 `2aa269b785eeb53a82983a20501ddf7c1d9c48e33ab63a41391ac6c9f7fb6987`
(from the [Hugging Face API](https://huggingface.co/api/models/ggml-org/whisper-vad?blobs=true)).
License: MIT, Silero Team ([snakers4/silero-vad](https://github.com/snakers4/silero-vad)); see `NOTICE`.
It is installed only by an explicit command, never during dictation:

```sh
python3 scripts/local-whisper.py --app murmur setup --vad-only
# or, offline, from a file you already have:
python3 scripts/local-whisper.py --app murmur import-model --vad ggml-silero-v6.2.0.bin \
  --sha256 2aa269b785eeb53a82983a20501ddf7c1d9c48e33ab63a41391ac6c9f7fb6987
```

Imports check the SHA-256 and the Silero GGML header (`silero-16k`) and keep the
previous VAD selection on failure. `start` refuses with a missing or corrupt VAD
model before it adopts or launches anything, so a running runtime, its state and
the selected models are left untouched; the supervisor re-verifies the file
before launching the engine. `status` reports `missing_vad_model` or
`corrupt_vad_model` and the selected VAD's version, SHA-256 and license.

**Upgrading an existing install:** after updating the scripts, run
`setup --vad-only` (or `import-model --vad`) before the next `start`, otherwise
start refuses. Login-item copies of the scripts (for example
`~/Library/Application Support/murmur/engine-scripts/`) must be refreshed with
both files, and the Murmur app must include the matching client change: older
builds reject the new `vad` capability field.

Real-model acceptance runs outside the offline tests with
`scripts/eval-local-vad.py` against two managed runtimes using the same model,
one from the previous scripts (VAD off) and one with VAD on.
