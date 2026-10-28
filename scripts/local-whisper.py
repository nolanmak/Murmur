#!/usr/bin/env python3
"""Explicit, offline-after-setup whisper.cpp development runtime (Python 3.9+)."""

import argparse
import contextlib
import fcntl
import hashlib
import http.server
import json
import os
from pathlib import Path
import secrets
import select
import shutil
import socket
import socketserver
import subprocess
import struct
import sys
import tempfile
import time
import urllib.request

from whisper_gateway import Gateway

REVISION = "5359861c739e955e79d9a303bcbc70fb988958b1"
MODEL_SHA256 = "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002"
MODEL_URL = (
    f"https://huggingface.co/ggerganov/whisper.cpp/resolve/{REVISION}/ggml-base.en.bin"
)
# Silero VAD for whisper.cpp (MIT): https://huggingface.co/ggml-org/whisper-vad
VAD_REVISION = "9ffd54a1e1ee413ddf265af9913beaf518d1639b"
VAD_SHA256 = "2aa269b785eeb53a82983a20501ddf7c1d9c48e33ab63a41391ac6c9f7fb6987"
VAD_URL = (
    f"https://huggingface.co/ggml-org/whisper-vad/resolve/{VAD_REVISION}/ggml-silero-v6.2.0.bin"
)
# Silero speech probability needed to decode. 0.8 (upstream default 0.5) drops
# the short music/noise segments Whisper turned into "Thank you." etc. while
# keeping speech in music at 10 dB SNR (Murmur #99 corpus sweep).
VAD_THRESHOLD = "0.8"
VAD_MISSING = (
    "missing or corrupt VAD model; run setup --vad-only or import-model --vad explicitly"
)
PORTS = {"murmur": 17863, "agentflow": 17864}


def emit(value):
    print(json.dumps(value), flush=True)


def write_json(path, value):
    temp = path.with_suffix(".tmp")
    temp.write_text(json.dumps(value))
    temp.chmod(0o600)
    temp.replace(path)


def read_json(path):
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError):
        return {}


def digest(path):
    with path.open("rb") as source:
        h = hashlib.sha256()
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def model_metadata(path):
    """Inspect selected-file metadata, not a claim about an HTTP server's model.

    whisper.cpp v1.9.1 src/whisper.cpp: whisper_model_load and whisper_vocab.
    Only the fixed-size GGML header is read; the engine still validates weights.
    Unknown vocabulary layouts fail closed rather than inventing capabilities.
    """
    with path.open("rb") as source:
        header = source.read(48)
    if len(header) != 48:
        raise ValueError("truncated Whisper GGML model header")
    (magic, vocab, audio_ctx, audio_state, audio_head, audio_layer, text_ctx,
     text_state, text_head, text_layer, mels, ftype) = struct.unpack("<12i", header)
    if magic != 0x67676D6C:
        raise ValueError("unsupported model format; expected Whisper GGML")
    if vocab not in (51864, 51865, 51866):
        raise ValueError("unsupported Whisper vocabulary layout")
    if (min(audio_ctx, audio_state, audio_head, audio_layer, text_ctx,
            text_state, text_head, text_layer, mels) <= 0
            or audio_state != text_state
            or audio_state % audio_head != 0
            or text_state % text_head != 0
            or ftype < 0):
        raise ValueError("invalid Whisper model dimensions")
    return {
        "format": "whisper-ggml",
        "vocabulary_size": vocab,
        "multilingual": vocab >= 51865,
        "language_count": vocab - 51766 if vocab >= 51865 else 1,
    }


def vad_metadata(path):
    """Check the Silero GGML header whisper.cpp v1.9.1 whisper_vad_init reads."""
    with path.open("rb") as source:
        header = source.read(30)
    if len(header) != 30:
        raise ValueError("truncated Silero VAD model header")
    magic, name_length = struct.unpack("<2i", header[:8])
    if magic != 0x67676D6C or name_length != 10 or header[8:18] != b"silero-16k":
        raise ValueError("unsupported VAD model; expected Silero GGML")
    return {"format": "silero-ggml", "version": "%d.%d.%d" % struct.unpack("<3i", header[18:])}


def selected_vad(state):
    """Return the verified selected VAD file, or raise before any engine work."""
    link = state / "vad.bin"
    try:
        path = link.resolve(strict=True)
        if digest(path) != path.stem:
            raise ValueError(VAD_MISSING)
        vad_metadata(path)
    except (OSError, ValueError):
        raise ValueError(VAD_MISSING) from None
    return path


def install(state, source, expected, vad=False):
    if len(expected) != 64 or any(c not in "0123456789abcdef" for c in expected):
        raise ValueError("sha256 must be 64 lowercase hexadecimal characters")
    fd, name = tempfile.mkstemp(prefix="model-", suffix=".partial", dir=state)
    try:
        with os.fdopen(fd, "wb") as target:
            shutil.copyfileobj(source, target)
            target.flush()
            os.fsync(target.fileno())
        if digest(Path(name)) != expected:
            raise ValueError("model checksum mismatch; existing model preserved")
        (vad_metadata if vad else model_metadata)(Path(name))
        folder, selected = ("vad", "vad.bin") if vad else ("models", "model.bin")
        (state / folder).mkdir(exist_ok=True, mode=0o700)
        os.replace(name, state / folder / (expected + ".bin"))
        link = state / (selected + ".next")
        link.unlink(missing_ok=True)
        link.symlink_to(Path(folder) / (expected + ".bin"))
        os.replace(link, state / selected)
    finally:
        with contextlib.suppress(FileNotFoundError):
            os.unlink(name)


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise ValueError("local runtime redirects are forbidden")


def request_local(url, token=None, timeout=0.5):
    # Ignore proxy environment: runtime control and readiness must stay on loopback.
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    request = urllib.request.Request(
        url, headers={"Authorization": "Bearer " + token} if token else {}
    )
    with opener.open(request, timeout=timeout) as response:
        return json.load(response)


def control(record, action):
    try:
        result = request_local(
            f"http://127.0.0.1:{int(record['control_port'])}/{action}", record["token"]
        )
        return result if result.get("token") == record["token"] else None
    except (OSError, ValueError, KeyError):
        return None


def ready(port, prefix=""):
    try:
        return request_local(f"http://127.0.0.1:{port}{prefix}/health").get("status") == "ok"
    except (OSError, ValueError):
        return False


def read_supervisor_token(fd, timeout=5):
    """Read a single bounded secret frame from the parent's private pipe."""
    deadline = time.monotonic() + timeout
    frame = bytearray()
    while len(frame) <= 65:
        remaining = deadline - time.monotonic()
        if remaining <= 0 or not select.select([fd], [], [], remaining)[0]:
            raise ValueError("supervisor secret handoff timed out")
        chunk = os.read(fd, 66 - len(frame))
        if not chunk:
            raise ValueError("supervisor secret handoff was incomplete")
        frame.extend(chunk)
        if b"\n" in frame:
            break
    if (
        len(frame) != 65
        or frame[-1:] != b"\n"
        or any(byte not in b"0123456789abcdef" for byte in frame[:-1])
    ):
        raise ValueError("invalid supervisor secret handoff")
    return frame[:-1].decode("ascii")


class LoopbackHTTPServer(http.server.HTTPServer):
    """Bind the literal loopback address without HTTPServer's reverse DNS lookup."""

    def server_bind(self):
        socketserver.TCPServer.server_bind(self)
        self.server_name, self.server_port = self.server_address[:2]


def supervise(state, binary, port, token, timeout):
    child = None
    stopping = False
    model = (state / "model.bin").resolve(strict=True)
    model_sha256 = digest(model)
    if model_sha256 != model.stem:
        raise ValueError("corrupt model before engine startup")
    metadata = model_metadata(model)
    vad = selected_vad(state)
    # Upstream has no inherited-listener option. A random private port/path,
    # owned child liveness and readiness prevent adopting a public endpoint.
    engine_port = 0
    prefix = "/" + secrets.token_hex(24)
    gateway = Gateway(port, engine_port, prefix, metadata, model_sha256,
                      lambda: bool(child and child.poll() is None and ready(engine_port, prefix)),
                      vad=True)
    port = gateway.server.server_port
    try:
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            engine_port = probe.getsockname()[1]
        gateway.engine_port = engine_port
    except BaseException:
        gateway.close()
        raise

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            nonlocal stopping
            if self.headers.get("Authorization") != "Bearer " + token:
                self.send_error(403)
                return
            if self.path == "/stop":
                stopping = True
                gateway.closed.set()
            elif self.path != "/status":
                self.send_error(404)
                return
            healthy = gateway.is_ready()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(
                json.dumps(
                    {
                        "token": token,
                        "ready": healthy,
                        "murmur_capabilities": gateway.capabilities if healthy else None,
                        "url": f"http://127.0.0.1:{port}",
                    }
                ).encode()
            )

        def log_message(self, *args):
            pass

    try:
        server = LoopbackHTTPServer(("127.0.0.1", 0), Handler)
    except BaseException:
        gateway.close()
        raise
    server.timeout = 0.2
    record = {"control_port": server.server_port, "token": token, "port": port}
    try:
        child = subprocess.Popen(
            [
                binary,
                "--host",
                "127.0.0.1",
                "--port",
                str(engine_port),
                "--request-path",
                prefix,
                "--public",
                str(state / "unused-public"),
                "-m",
                str(model),
                "--vad",
                "--vad-model",
                str(vad),
                "--vad-threshold",
                VAD_THRESHOLD,
            ],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        write_json(state / "runtime.json", record)
        deadline = time.monotonic() + timeout
        became_ready = False
        while not stopping and not gateway.failed.is_set() and child.poll() is None:
            became_ready = became_ready or gateway.is_ready()
            if not became_ready and time.monotonic() >= deadline:
                break
            server.handle_request()
    finally:
        gateway.closed.set()
        if child and child.poll() is None:
            child.terminate()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
        server.server_close()
        gateway.close()
        if read_json(state / "runtime.json").get("token") == token:
            (state / "runtime.json").unlink(missing_ok=True)
        write_json(
            state / "lifecycle.json",
            {"reason": "stopped" if stopping else "engine_exited_or_unready"},
        )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", choices=PORTS, required=True)
    parser.add_argument("--state-dir", type=Path)
    commands = parser.add_subparsers(dest="command", required=True)
    setup = commands.add_parser(
        "setup", help="explicitly download and verify pinned base.en and Silero VAD models"
    )
    setup.add_argument(
        "--vad-only", action="store_true", help="install only the pinned VAD model"
    )
    imp = commands.add_parser("import-model")
    imp.add_argument("path", type=Path)
    imp.add_argument("--sha256", required=True)
    imp.add_argument("--vad", action="store_true", help="import a Silero VAD model")
    start = commands.add_parser("start")
    start.add_argument("--binary", default="whisper-server")
    start.add_argument("--port", type=int)
    start.add_argument("--timeout", type=float, default=30)
    commands.add_parser("status")
    commands.add_parser("stop")
    internal = commands.add_parser("_supervise", help=argparse.SUPPRESS)
    internal.add_argument("binary")
    internal.add_argument("port", type=int)
    internal.add_argument("timeout", type=float)
    args = parser.parse_args()
    state = (
        (
            args.state_dir
            or Path.home()
            / "Library"
            / "Application Support"
            / args.app
            / "local-whisper"
        )
        .expanduser()
        .resolve()
    )
    state.mkdir(parents=True, exist_ok=True, mode=0o700)
    state.chmod(0o700)
    if args.command == "_supervise":
        token = read_supervisor_token(sys.stdin.fileno())
        sys.stdin.close()
        supervise(state, args.binary, args.port, token, args.timeout)
        return
    with (state / "lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        record = read_json(state / "runtime.json")
        if args.command == "status":
            result = control(record, "status")
            model = state / "model.bin"
            metadata = None
            reason = read_json(state / "lifecycle.json").get("reason", "stopped")
            if not model.is_file():
                reason = "missing_model"
            elif digest(model) != model.resolve().stem:
                reason = "corrupt_model"
            else:
                try:
                    metadata = model_metadata(model)
                    metadata["sha256"] = model.resolve().stem
                except ValueError:
                    reason = "unsupported_model"
                else:
                    if not (state / "vad.bin").exists():
                        reason = "missing_vad_model"
                    elif not shutil.which("whisper-server"):
                        reason = "missing_binary"
            vad = None
            try:
                vad = vad_metadata(selected_vad(state))
                vad["sha256"] = (state / "vad.bin").resolve().stem
                vad["license"] = "MIT (Silero VAD)" if vad["sha256"] == VAD_SHA256 else "unknown"
            except ValueError:
                if reason not in ("missing_model", "corrupt_model", "unsupported_model",
                                  "missing_vad_model"):
                    reason = "corrupt_vad_model"
            public = {k: v for k, v in (result or {}).items() if k != "token"}
            public.update(
                {
                    "owned": bool(result),
                    "ready": bool(result and result["ready"]),
                    "reason": ("ready" if result["ready"] else "loading")
                    if result
                    else reason,
                    "tested_runtime": "whisper.cpp 1.9.1",
                    "model": str(model),
                    "selected_model_metadata": metadata,
                    "selected_vad_metadata": vad,
                }
            )
            emit(public)
        elif args.command == "stop":
            result = control(record, "stop")
            deadline = time.monotonic() + 7
            while (
                result
                and (state / "runtime.json").exists()
                and time.monotonic() < deadline
            ):
                time.sleep(0.05)
            if result and (state / "runtime.json").exists():
                raise ValueError("owned runtime did not stop within deadline")
            emit({"ready": False, "stopped": bool(result)})
        elif args.command in ("setup", "import-model"):
            if control(record, "status"):
                raise ValueError("stop owned runtime before replacing model")
            if args.command == "setup":
                if not args.vad_only:
                    with urllib.request.urlopen(MODEL_URL, timeout=60) as source:
                        install(state, source, MODEL_SHA256)
                with urllib.request.urlopen(VAD_URL, timeout=60) as source:
                    install(state, source, VAD_SHA256, vad=True)
            else:
                with args.path.open("rb") as source:
                    install(state, source, args.sha256, vad=args.vad)
            result = {}
            for key, name in (("model", "model.bin"), ("vad_model", "vad.bin")):
                if (state / name).is_file():
                    result[key] = str(state / name)
                    result[key + "_sha256"] = digest(state / name)
            emit(result)
        elif args.command == "start":
            # Checked before adopting a running instance: a refused start never
            # touches the runtime, selection or state already in place.
            selected_vad(state)
            existing = control(record, "status")
            if existing:
                emit({k: v for k, v in existing.items() if k != "token"})
                return
            model = state / "model.bin"
            expected = model.resolve().stem
            if not model.is_file() or digest(model) != expected:
                raise ValueError(
                    "missing or corrupt model; run setup or import-model explicitly"
                )
            model_metadata(model)
            binary = shutil.which(args.binary)
            if not binary:
                raise ValueError(
                    "whisper-server missing; install with brew install whisper-cpp, or pass --binary PATH (tested 1.9.1)"
                )
            port = args.port or PORTS[args.app]
            if not 1 <= port <= 65535 or not 0 < args.timeout <= 300:
                raise ValueError(
                    "port must be 1..65535 and timeout must be >0 and <=300 seconds"
                )
            with socket.socket() as probe:
                probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                try:
                    probe.bind(("127.0.0.1", port))
                except OSError:
                    write_json(state / "lifecycle.json", {"reason": "port_conflict"})
                    raise ValueError(
                        "port already in use; choose --port or use external server mode"
                    ) from None
            token = secrets.token_hex(32)
            supervisor = subprocess.Popen(
                [
                    sys.executable,
                    str(Path(__file__).resolve()),
                    "--app",
                    args.app,
                    "--state-dir",
                    str(state),
                    "_supervise",
                    binary,
                    str(port),
                    str(args.timeout),
                ],
                start_new_session=True,
                stdin=subprocess.PIPE,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            supervisor.stdin.write((token + "\n").encode("ascii"))
            supervisor.stdin.close()
            deadline = time.monotonic() + args.timeout
            while time.monotonic() < deadline and supervisor.poll() is None:
                record = read_json(state / "runtime.json")
                result = (
                    control(record, "status") if record.get("token") == token else None
                )
                if result and result["ready"]:
                    emit({k: v for k, v in result.items() if k != "token"})
                    return
                time.sleep(0.1)
            if record.get("token") == token:
                control(record, "stop")
            with contextlib.suppress(subprocess.TimeoutExpired):
                supervisor.wait(timeout=7)
            raise ValueError(
                "local engine failed readiness deadline; verify model and whisper-server binary"
            )


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError) as error:
        emit({"error": str(error)})
        sys.exit(1)
