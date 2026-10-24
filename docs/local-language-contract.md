# Local language capability contract (client implemented; runtime pending)

Murmur cannot infer a running server's capabilities from a selected file on disk.
The optional version-1 extension below binds preflight and inference to a serving
instance/model. The current `local-whisper.py` helper does **not** implement this
HTTP extension yet; its `selected_model_metadata` is diagnostic file metadata.
Do not advertise that metadata as live capabilities without owning the loaded
model and enforcing the request binding.

A successful `GET /health` response may include:

```json
{
  "status": "ok",
  "murmur_capabilities": {
    "protocol_version": 1,
    "instance_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "model_sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    "languages": ["en", "fr"]
  }
}
```

Both identifiers are 64 lowercase hexadecimal characters. Each serving instance
must generate a fresh ID at startup; model replacement must change the binding.
The languages are explicit canonical codes supported by the *loaded* model and
runtime, not a filename inference. The list is nonempty, unique and at most 128
entries. The client accepts at most 16 KiB for the entire response under the
existing two-second capture preflight deadline. Unknown protocol versions or
malformed capability objects fail closed, including for English.

The client sends `X-Murmur-Instance` and `X-Murmur-Model` with inference, alongside
the multipart language, `translate=false`, `detect_language=false`, JSON response
format and WAV. Before using any audio, a conforming adapter must atomically
validate the binding and language against the model used for inference. It must
refuse a mismatch (409) or unsupported language (422); it must not reload a model,
coerce language, enable translation or contact a cloud provider. Successful
responses echo the *actual* instance and model headers. Missing/different headers
cause Murmur to discard the result. Echoing arbitrary request headers without
checking the running model is not a conforming implementation.

Model loading/replacement and inference require serialization or immutable model
ownership. Health can report ready only after that model has loaded successfully.
A future managed adapter must prevent bypassing that model ownership through the
upstream `/load` route. These IDs fence accidental lifecycle changes; they are
not authentication against another process running as the same user.

With no capability extension, only explicit English is accepted. Whisper's
models support English, and the request explicitly pins it instead of relying on
the server's default. This legacy route cannot attest the serving model identity;
non-English selection requires the stronger contract. No automatic detection or
regional-code normalization is performed.

Tests in `tests/local_stt.rs`, `tests/capture.rs` and `tests/fallback.rs` implement
synthetic adapter responses to verify the client. Actual managed multilingual
model loading/inference, shared capability extraction and native language UI
remain open acceptance work for #9 and FlyOnTheWall #119/#118.
