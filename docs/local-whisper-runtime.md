# Local Whisper development runtime

Requires Python 3.9+ and a separately installed `whisper-server`. Tested with whisper.cpp **1.9.1** on Apple Silicon. Install deliberately with `brew install whisper-cpp` (Homebrew may supply a newer version), or build upstream v1.9.1 and pass `start --binary '/path with spaces/whisper-server'`. The helper never installs executables or implicitly downloads models. No Python packages are required.

From the repository root, substituting `agentflow` for `murmur` where appropriate:

```sh
python3 scripts/local-whisper.py --app murmur setup
python3 scripts/local-whisper.py --app murmur start
python3 scripts/local-whisper.py --app murmur status
python3 scripts/local-whisper.py --app murmur stop
```

`setup` explicitly downloads base.en (~148 MB), verifies its SHA-256, then atomically selects it. Alternatively, import an existing model:

```sh
python3 scripts/local-whisper.py --app murmur import-model '/path/to/ggml-base.en.bin' --sha256 a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002
```

Custom models require their known SHA-256. Runtime start rechecks integrity offline. Corrupt or interrupted imports preserve the previous model selection; verified immutable digest-named files are selected through an atomic symlink replacement. Old verified models remain on disk for manual removal while stopped. Stop before changing the selected model. State defaults to `~/Library/Application Support/<app>/local-whisper`; override via `--state-dir PATH` before the subcommand. State is private to the current user.

Murmur defaults to `http://127.0.0.1:17863`; Agentflow uses `http://127.0.0.1:17864`. `start --port N --timeout 30` overrides the port and readiness deadline (maximum 300 seconds). Configure the app's local transcription endpoint to match. Start is idempotent for an already-owned runtime, retaining its original configuration; stop before changing binary or port.

A private authenticated loopback supervisor owns the child process. Its random bearer token is handed over through a private stdin pipe with a five-second deadline and strict frame-size/format validation; it never appears in process arguments or public CLI JSON. Stop contacts this supervisor and terminates only its own child, never a PID from a stale state file. Runtime output is discarded to avoid storing transcripts in logs. Readiness and control ignore HTTP proxy variables and reject redirects. A taken port reports an error rather than adopting or stopping another server. To use an external server, configure its loopback endpoint in the app and manage that process yourself; helper stop will not affect it. These are development CLI controls, not a bundled production installer or automatic app-exit hook. App and helper lifecycle remain explicit.

Setup requires internet. Start, status, stop and transcription can run offline after model installation. Local transcription does not make unrelated cloud LLM/TTS features local.

Pinned model source: [ggerganov/whisper.cpp revision 5359861c739e955e79d9a303bcbc70fb988958b1](https://huggingface.co/ggerganov/whisper.cpp/tree/5359861c739e955e79d9a303bcbc70fb988958b1), file `ggml-base.en.bin`. SHA and size (147,964,211 bytes) verified from the [Hugging Face model API](https://huggingface.co/api/models/ggerganov/whisper.cpp/revision/5359861c739e955e79d9a303bcbc70fb988958b1?blobs=true). Runtime source: [whisper.cpp v1.9.1](https://github.com/ggml-org/whisper.cpp/tree/v1.9.1).

Tests: `python3 -m unittest discover -s scripts -p 'test_local_whisper.py'` (9 passed locally in 3.8 seconds). Initial four acceptance tests failed before implementation; all nine now pass. Coverage includes verified imports, corrupt/interrupted replacement preservation, corrupted model startup, missing model, authenticated idempotent lifecycle, paths with spaces, external port collision, stale records, exit-zero/unready engines, readiness timeout cleanup, rejected redirects, and bounded secret handoff. The lifecycle test reads the actual process list and verifies that the secret is absent while authenticated status and stop still work. Real installed 1.9.1/base.en import → start → ready → stop was also exercised. Audio accuracy and end-to-end app insertion/submission require separate app QA.

CI timing: the successful-engine fixture allows a 30-second readiness deadline and 60 seconds for each CLI subprocess, accommodating cold macOS runner startup. The deliberate unready-engine case retains its 0.5-second readiness deadline; readiness, process ownership, port collision, and timeout cleanup assertions are unchanged. The process-list token assertion failed before the private-pipe fix and passed afterward.
