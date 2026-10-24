# Managed local language capability contract

Murmur cannot infer a running server's capabilities from a selected file on disk.
The optional version-1 extension below binds preflight and inference to a serving
instance/model. `local-whisper.py` implements it through `whisper_gateway.py`
for the tested whisper.cpp 1.9.1 runtime. The helper owns one fixed-model child;
the public port serves only health and inference. Its separate
`selected_model_metadata` remains diagnostic file metadata. Live capabilities
are advertised only after the owned child loads the selected digest-named file
and passes readiness.

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
The managed gateway rejects `/load` and all other public routes. It starts the
child on a separate internal loopback port with a random request-path prefix;
that endpoint is not exposed in the public API. The helper never forwards model
replacement, prompts, translation or arbitrary inference parameters. Stop is
required before selecting another model, and restart creates a new instance ID.
Internal engine routes are implementation details, not another supported API.
These IDs fence accidental lifecycle changes; they are not authentication
against a process running as the same user that deliberately inspects process
arguments or modifies model files. Custom binaries must implement the tested
whisper.cpp 1.9.1 behavior; unrelated runtimes are not attested by this helper.

With no capability extension, only explicit English is accepted. Whisper's
models support English, and the request explicitly pins it instead of relying on
the server's default. This legacy route cannot attest the serving model identity;
non-English selection requires the stronger contract. No automatic detection or
regional-code normalization is performed.

Tests in `tests/local_stt.rs`, `tests/capture.rs` and `tests/fallback.rs` implement
synthetic adapter responses to verify the client. Python lifecycle tests exercise
the actual gateway, private backend routing, unsupported languages, stale
bindings, restart, busy refusal, oversized input, invalid parameters and Stop
while inference is blocked. A timed-out, oversized or malformed backend response
retires the child rather than queuing work behind inference whose completion is
unknown.

The gateway admits at most four HTTP workers and one inference at a time; it
has no inference queue. Uploads have a three-second body deadline and a 4 MiB
limit. Multipart parsing is limited to five fields and 2 KiB field headers.
Only bounded mono PCM16/16 kHz WAV is accepted; it is normalized in memory before
forwarding. Inference has a 180-second deadline and a 1 MiB response limit.
Audio and transcripts are not written to disk or logged. Stop closes admission
before terminating its child and discards an in-flight result.

Real-engine smoke checks on 2026-10-24 used whisper.cpp 1.9.1, the Rust transport
and loopback-only network sandboxing. Installed small.en transcribed synthetic
English and refused French with 422. Multilingual tiny from model revision
`5359861c739e955e79d9a303bcbc70fb988958b1`, SHA-256
`be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21`,
loaded 99-language capabilities and transcribed synthetic French with the
explicit `fr` setting. Test runtimes, downloaded weights and audio were removed.
No microphone or real provider credentials were used. This verifies adapter
integration, not human speech quality, resource benchmarks or native insertion.
Shared capability extraction, native language UI and the paired evaluation
remain open for #9 and FlyOnTheWall #119/#118.
