import hashlib, json, pathlib, socket, subprocess, sys, tempfile, unittest
import importlib.util, io, http.server, threading, os, struct, http.client, wave, time
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location(
    "runtime", pathlib.Path(__file__).with_name("local-whisper.py")
)
runtime = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runtime)
SCRIPT = pathlib.Path(__file__).with_name("local-whisper.py")


FIXTURE_ENGINE = r"""
import argparse, http.server, socketserver, json, pathlib, time
p = argparse.ArgumentParser()
p.add_argument("--port", type=int)
p.add_argument("-m")
p.add_argument("--request-path", default="")
a, _ = p.parse_known_args()
class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200 if self.path == a.request_path + "/health" else 404)
        self.end_headers()
        self.wfile.write(b'{"status":"ok"}')
    def do_POST(self):
        data = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        if self.path != a.request_path + "/inference":
            self.send_response(404); self.end_headers(); self.wfile.write(b'{}'); return
        assert b'name="translate"\r\n\r\nfalse' in data
        assert b'name="detect_language"\r\n\r\nfalse' in data
        language = data.split(b'name="language"\r\n\r\n')[1].split(b'\r\n')[0].decode()
        state = pathlib.Path(a.m).parent.parent
        (state / "inference-entered").touch()
        while (state / "hold-inference").exists(): time.sleep(0.01)
        body = json.dumps({"text":"fixture " + language}).encode()
        self.send_response(200); self.send_header("Content-Length", str(len(body)))
        self.end_headers(); self.wfile.write(body)
    def log_message(self, *args): pass
socketserver.ThreadingTCPServer(("127.0.0.1", a.port), Handler).serve_forever()
"""


def model_fixture(n_vocab=51864):
    # Valid tiny-model header, synthetic body: only the fake engine loads this.
    return struct.pack("<12i", 0x67676d6c, n_vocab, 1500, 384, 6, 4,
                       448, 384, 6, 4, 80, 1) + b"fixture weights"


class RuntimeTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="local whisper test ")
        self.root = pathlib.Path(self.tmp.name)
        self.model = self.root / "source model.bin"
        self.model.write_bytes(model_fixture())
        self.digest = hashlib.sha256(self.model.read_bytes()).hexdigest()

    def tearDown(self):
        self.run_cli("stop")
        self.tmp.cleanup()

    def run_cli(self, *args):
        return subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--app",
                "murmur",
                "--state-dir",
                str(self.root / "state folder"),
                *args,
            ],
            capture_output=True,
            text=True,
            timeout=60,
        )

    def install(self):
        r = self.run_cli("import-model", str(self.model), "--sha256", self.digest)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def start_fixture_engine(self, multilingual=False):
        if multilingual:
            self.model.write_bytes(model_fixture(51865))
            self.digest = hashlib.sha256(self.model.read_bytes()).hexdigest()
        self.install()
        fake = self.root / "fixture engine"
        fake.write_text("#!" + sys.executable + "\n" + FIXTURE_ENGINE)
        fake.chmod(0o755)
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            port = probe.getsockname()[1]
        result = self.run_cli("start", "--binary", str(fake), "--port", str(port), "--timeout", "30")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return port

    def health(self, port):
        conn = http.client.HTTPConnection("127.0.0.1", port, timeout=3)
        try:
            conn.request("GET", "/health")
            response = conn.getresponse()
            self.assertEqual(response.status, 200)
            return json.loads(response.read())
        finally:
            conn.close()

    def infer(self, port, caps, language="en", extra=(), path="/inference"):
        audio = io.BytesIO()
        with wave.open(audio, "wb") as wav:
            wav.setparams((1, 2, 16000, 0, "NONE", "not compressed"))
            wav.writeframes(struct.pack("<160h", *([1234] * 160)))
        fields = [("file", audio.getvalue()), ("language", language.encode()),
                  ("response_format", b"json"), *extra]
        body = b""
        for name, value in fields:
            body += ("--fixture-boundary\r\nContent-Disposition: form-data; name=\"" + name
                     + ("\"; filename=\"audio.wav" if name == "file" else "")
                     + "\"\r\n\r\n").encode() + value + b"\r\n"
        body += b"--fixture-boundary--\r\n"
        conn = http.client.HTTPConnection("127.0.0.1", port, timeout=3)
        try:
            conn.request("POST", path, body, {"Content-Type":"multipart/form-data; boundary=fixture-boundary",
                "X-Murmur-Instance":caps["instance_id"], "X-Murmur-Model":caps["model_sha256"]})
            response = conn.getresponse()
            return response.status, dict(response.getheaders()), json.loads(response.read())
        finally:
            conn.close()

    def test_managed_runtime_advertises_and_enforces_loaded_model_language(self):
        port = self.start_fixture_engine(multilingual=True)
        health = self.health(port)
        self.assertIn("murmur_capabilities", health)
        caps = health["murmur_capabilities"]
        self.assertEqual(caps["model_sha256"], self.digest)
        self.assertEqual(json.loads(self.run_cli("status").stdout)["murmur_capabilities"], caps)
        self.assertEqual(len(caps["languages"]), 99)
        self.assertIn("fr", caps["languages"])
        self.assertNotIn("yue", caps["languages"])
        status, headers, body = self.infer(port, caps, "fr")
        self.assertEqual(status, 200, body)
        self.assertEqual(body["text"], "fixture fr")
        self.assertEqual(headers["X-Murmur-Instance"], caps["instance_id"])
        self.assertEqual(headers["X-Murmur-Model"], self.digest)

    def test_gateway_refuses_language_binding_and_parameter_changes_without_poisoning_engine(self):
        port = self.start_fixture_engine()
        caps = self.health(port)["murmur_capabilities"]
        self.assertEqual(caps["languages"], ["en"])
        entered = self.root / "state folder/inference-entered"
        for key in ("instance_id", "model_sha256"):
            changed = dict(caps, **{key: "0" * 64})
            self.assertEqual(self.infer(port, changed)[0], 409)
        self.assertEqual(self.infer(port, caps, "fr")[0], 422)
        self.assertEqual(self.infer(port, caps, path="/load")[0], 404)
        self.assertEqual(self.infer(port, caps, extra=[("language", b"fr")])[0], 400)
        self.assertEqual(self.infer(port, caps, extra=[("translate", b"true")])[0], 422)
        self.assertEqual(self.infer(port, caps, extra=[("detect_language", b"true")])[0], 422)
        self.assertEqual(self.infer(port, caps, extra=[("prompt", b"private fixture")])[0], 400)
        self.assertFalse(entered.exists(), "rejected requests must not reach inference")
        self.assertEqual(self.infer(port, caps)[0], 200)
        self.assertTrue(entered.exists())
        self.assertEqual(self.health(port)["murmur_capabilities"], caps)

    def test_gateway_restart_changes_instance_and_refuses_old_preflight(self):
        port = self.start_fixture_engine()
        old = self.health(port)["murmur_capabilities"]
        self.assertEqual(self.run_cli("stop").returncode, 0)
        port = self.start_fixture_engine(multilingual=True)
        new = self.health(port)["murmur_capabilities"]
        self.assertNotEqual(new["instance_id"], old["instance_id"])
        self.assertNotEqual(new["model_sha256"], old["model_sha256"])
        self.assertEqual(self.infer(port, old)[0], 409)
        self.assertEqual(self.infer(port, new, "fr")[0], 200)

    def test_gateway_releases_inference_before_a_completed_response_handler_returns(self):
        for language, expected in (("en", 200), ("fr", 422)):
            with self.subTest(language=language):
                gateway = runtime.Gateway(0, 1, "/unused", {"language_count": 1}, "b" * 64, lambda: True)
                paused, release, returned = threading.Event(), threading.Event(), threading.Event()
                handler = gateway.server.RequestHandlerClass
                reply = handler.reply
                def delayed_reply(request, status, body, bound=False):
                    reply(request, status, body, bound)
                    if status == expected and not paused.is_set():
                        paused.set()
                        release.wait(5)
                        returned.set()
                try:
                    with patch.object(handler, "reply", delayed_reply), patch.object(gateway, "transcribe", return_value={"text": "fixture en"}):
                        port, caps = gateway.server.server_port, gateway.capabilities
                        self.assertEqual(self.infer(port, caps, language)[0], expected)
                        self.assertTrue(paused.wait(2), "first response was not held after delivery")
                        self.assertEqual(self.infer(port, caps)[0], 200,
                                         "a fully received response must release inference admission")
                finally:
                    release.set()
                    if paused.is_set():
                        self.assertTrue(returned.wait(2))
                    gateway.close()

    def test_gateway_busy_refuses_queue_and_stop_discards_inflight_result(self):
        port = self.start_fixture_engine()
        caps = self.health(port)["murmur_capabilities"]
        state = self.root / "state folder"
        (state / "hold-inference").touch()
        outcomes = []
        def infer():
            try:
                outcomes.append(self.infer(port, caps)[0])
            except (OSError, http.client.HTTPException):
                outcomes.append("disconnected")
        worker = threading.Thread(target=infer)
        worker.start()
        deadline = time.monotonic() + 3
        while not (state / "inference-entered").exists() and time.monotonic() < deadline:
            time.sleep(0.01)
        self.assertTrue((state / "inference-entered").exists())
        self.assertEqual(self.infer(port, caps)[0], 429)
        self.assertEqual(self.health(port)["murmur_capabilities"], caps)
        stopped_at = time.monotonic()
        self.assertEqual(self.run_cli("stop").returncode, 0)
        self.assertLess(time.monotonic() - stopped_at, 7)
        worker.join(timeout=4)
        self.assertFalse(worker.is_alive())
        self.assertNotIn(200, outcomes)
        self.assertFalse((state / "runtime.json").exists())

    def test_gateway_rejects_oversized_request_before_receiving_audio(self):
        port = self.start_fixture_engine()
        caps = self.health(port)["murmur_capabilities"]
        conn = http.client.HTTPConnection("127.0.0.1", port, timeout=3)
        try:
            conn.putrequest("POST", "/inference")
            conn.putheader("X-Murmur-Instance", caps["instance_id"])
            conn.putheader("X-Murmur-Model", caps["model_sha256"])
            conn.putheader("Content-Length", str(4 * 1024 * 1024 + 1))
            conn.endheaders()
            response = conn.getresponse()
            self.assertEqual(response.status, 413)
            response.read()
        finally:
            conn.close()
        self.assertFalse((self.root / "state folder/inference-entered").exists())

    def test_gateway_retires_engine_after_timeout_oversize_or_malformed_response(self):
        for mode in ("slow", "oversize", "malformed"):
            calls = []
            class Backend(http.server.BaseHTTPRequestHandler):
                def do_POST(self):
                    calls.append(self.path)
                    self.rfile.read(int(self.headers["Content-Length"]))
                    body = b"x" * (1024 * 1024 + 1) if mode == "oversize" else b"not json"
                    self.send_response(200)
                    self.send_header("Content-Length", "100" if mode == "slow" else str(len(body)))
                    self.end_headers()
                    try:
                        if mode == "slow":
                            for _ in range(20):
                                time.sleep(0.05)
                                self.wfile.write(b"x")
                                self.wfile.flush()
                        else:
                            self.wfile.write(body)
                    except OSError:
                        pass
                def log_message(self, *args): pass
            backend = runtime.LoopbackHTTPServer(("127.0.0.1", 0), Backend)
            thread = threading.Thread(target=backend.serve_forever, kwargs={"poll_interval":0.01}, daemon=True)
            thread.start()
            gateway = runtime.Gateway(0, backend.server_port, "/private", {"language_count":1}, "b" * 64, lambda: True)
            try:
                with patch.dict(runtime.Gateway.transcribe.__globals__, {"INFERENCE_SECONDS":0.2}):
                    start = time.monotonic()
                    status, _, _ = self.infer(gateway.server.server_port, gateway.capabilities)
                    self.assertIn(status, (502, 504))
                    self.assertLess(time.monotonic() - start, 2)
                self.assertTrue(gateway.failed.is_set())
                self.assertEqual(self.infer(gateway.server.server_port, gateway.capabilities)[0], 503)
                self.assertEqual(calls, ["/private/inference"])
            finally:
                gateway.close()
                backend.shutdown()
                backend.server_close()
                thread.join(timeout=2)

    def test_loopback_supervisor_startup_never_resolves_a_hostname(self):
        self.install()
        state = self.root / "state folder"
        child = Mock()
        child.poll.return_value = 0
        with patch.object(socket, "getfqdn", side_effect=AssertionError("DNS must not be needed")):
            with patch.object(runtime.subprocess, "Popen", return_value=child):
                runtime.supervise(state, "fixture-engine", 0, "a" * 64, 0.5)
        self.assertFalse((state / "runtime.json").exists())

    def test_interrupted_import_preserves_selection(self):
        self.install()
        state = self.root / "state folder"
        before = (state / "model.bin").read_bytes()

        class Broken(io.BytesIO):
            def read(self, *args):
                raise OSError("interrupted download")

        with self.assertRaises(OSError):
            runtime.install(state, Broken(b"new"), "0" * 64)
        self.assertEqual((state / "model.bin").read_bytes(), before)
        self.assertFalse(list(state.glob("*.partial")))

    def test_redirect_never_follows_or_sends_token_to_destination(self):
        calls = []

        class Destination(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                calls.append(self.headers)
                self.send_response(200)
                self.end_headers()
                self.wfile.write(b"{}")

            def log_message(self, *args):
                pass

        target = runtime.LoopbackHTTPServer(("127.0.0.1", 0), Destination)

        class Redirect(Destination):
            def do_GET(self):
                self.send_response(302)
                self.send_header("Location", f"http://127.0.0.1:{target.server_port}/")
                self.end_headers()

        redirect = runtime.LoopbackHTTPServer(("127.0.0.1", 0), Redirect)
        for server in (target, redirect):
            threading.Thread(target=server.serve_forever, daemon=True).start()
        try:
            with self.assertRaises(ValueError):
                runtime.request_local(
                    f"http://127.0.0.1:{redirect.server_port}/", "private-token"
                )
            self.assertEqual(calls, [])
        finally:
            for server in (target, redirect):
                server.shutdown()
                server.server_close()

    def test_bad_import_preserves_model(self):
        self.install()
        before = (self.root / "state folder/model.bin").read_bytes()
        r = self.run_cli("import-model", str(self.model), "--sha256", "0" * 64)
        self.assertNotEqual(r.returncode, 0)
        self.assertEqual((self.root / "state folder/model.bin").read_bytes(), before)

    def test_checksum_valid_non_model_import_preserves_selection(self):
        self.install()
        selected = self.root / "state folder/model.bin"
        before = selected.read_bytes()
        invalid_dimensions = []
        for index, value in ((4, 0), (7, 256), (8, 7), (11, -1)):
            fields = list(struct.unpack("<12i", model_fixture()[:48]))
            fields[index] = value
            invalid_dimensions.append(struct.pack("<12i", *fields))
        for payload in [b"not a model", model_fixture()[:47],
                        b"GGUF" + model_fixture()[4:], model_fixture(123),
                        *invalid_dimensions]:
            self.model.write_bytes(payload)
            checksum = hashlib.sha256(payload).hexdigest()
            r = self.run_cli("import-model", str(self.model), "--sha256", checksum)
            self.assertNotEqual(r.returncode, 0, r.stdout)
            self.assertEqual(selected.read_bytes(), before)
            self.assertFalse(list(selected.parent.glob("*.partial")))

    def test_selected_model_language_metadata_comes_from_header(self):
        # Deliberately give every model the same misleading .en filename.
        misleading = self.root / "multilingual.en.bin"
        for n_vocab, multilingual, count in ((51864, False, 1),
                                             (51865, True, 99),
                                             (51866, True, 100)):
            payload = model_fixture(n_vocab)
            misleading.write_bytes(payload)
            checksum = hashlib.sha256(payload).hexdigest()
            r = self.run_cli("import-model", str(misleading), "--sha256", checksum)
            self.assertEqual(r.returncode, 0, r.stdout)
            status = json.loads(self.run_cli("status").stdout)
            metadata = status["selected_model_metadata"]
            self.assertEqual(metadata["multilingual"], multilingual)
            self.assertEqual(metadata["language_count"], count)
            self.assertEqual(metadata["sha256"], checksum)
            self.assertFalse(status["ready"])

    def test_legacy_invalid_model_fails_before_launch_and_status_explains(self):
        state = self.root / "state folder"
        models = state / "models"
        models.mkdir(parents=True)
        payload = b"checksum valid legacy non-model"
        checksum = hashlib.sha256(payload).hexdigest()
        selected = models / (checksum + ".bin")
        selected.write_bytes(payload)
        (state / "model.bin").symlink_to(selected)
        marker = self.root / "engine-started"
        fake = self.root / "engine"
        fake.write_text("#!" + sys.executable + "\nfrom pathlib import Path\n"
                        + "Path(" + repr(str(marker)) + ").touch()\n")
        fake.chmod(0o755)
        result = self.run_cli("start", "--binary", str(fake), "--timeout", "0.5")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("model", result.stdout)
        self.assertFalse(marker.exists())
        status = json.loads(self.run_cli("status").stdout)
        self.assertEqual(status["reason"], "unsupported_model")
        self.assertIsNone(status["selected_model_metadata"])
        self.assertFalse(status["ready"])

    def test_missing_model_never_starts(self):
        r = self.run_cli("start")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("model", r.stdout)

    def test_corruption_detected_at_start(self):
        self.install()
        (self.root / "state folder/model.bin").write_bytes(b"corrupt")
        r = self.run_cli("start")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("corrupt", r.stdout)

    def test_startup_exit_zero_and_timeout_fail_without_runtime(self):
        self.install()
        for body in ("pass", "import time;time.sleep(60)"):
            fake = self.root / "unready engine"
            fake.write_text("#!" + sys.executable + "\n" + body + "\n")
            fake.chmod(0o755)
            with socket.socket() as sock:
                sock.bind(("127.0.0.1", 0))
                port = sock.getsockname()[1]
            r = self.run_cli(
                "start", "--binary", str(fake), "--port", str(port), "--timeout", "0.5"
            )
            self.assertNotEqual(r.returncode, 0, r.stdout)
            self.assertFalse(json.loads(self.run_cli("status").stdout)["ready"])
            self.assertFalse((self.root / "state folder/runtime.json").exists())

    def test_supervisor_secret_handoff_is_bounded_and_validated(self):
        for frame in (b"bad\n", b"g" * 64 + b"\n", b"a" * 64, b"a" * 66):
            with self.subTest(length=len(frame)):
                reader, writer = os.pipe()
                try:
                    os.write(writer, frame)
                    os.close(writer)
                    with self.assertRaises(ValueError):
                        runtime.read_supervisor_token(reader, timeout=0.1)
                finally:
                    os.close(reader)
        reader, writer = os.pipe()
        try:
            with self.assertRaisesRegex(ValueError, "timed out"):
                runtime.read_supervisor_token(reader, timeout=0.05)
        finally:
            os.close(reader)
            os.close(writer)

    def test_stale_record_cannot_kill_unrelated_process(self):
        state = self.root / "state folder"
        state.mkdir()
        (state / "runtime.json").write_text(
            json.dumps({"port": 1, "token": "stale", "pid": 1})
        )
        r = self.run_cli("stop")
        self.assertEqual(r.returncode, 0, r.stdout)

    def test_lifecycle_path_spaces_and_port_conflict(self):
        self.install()
        fake = self.root / "fake whisper server"
        fake.write_text(
            "#!"
            + sys.executable
            + '\nimport argparse,http.server,socketserver\np=argparse.ArgumentParser();p.add_argument("--port",type=int);a,_=p.parse_known_args()\nclass H(http.server.BaseHTTPRequestHandler):\n def do_GET(self):\n  self.send_response(200);self.end_headers();self.wfile.write(b"{\\"status\\":\\"ok\\"}")\n def log_message(self,*a):pass\nsocketserver.TCPServer(("127.0.0.1",a.port),H).serve_forever()\n'
        )
        fake.chmod(0o755)
        with socket.socket() as s:
            s.bind(("127.0.0.1", 0))
            port = s.getsockname()[1]
        args = ("start", "--binary", str(fake), "--port", str(port), "--timeout", "30")
        r = self.run_cli(*args)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        first = json.loads(r.stdout)
        self.assertTrue(first["ready"])
        token = json.loads((self.root / "state folder/runtime.json").read_text())[
            "token"
        ]
        commands = subprocess.check_output(["ps", "-axo", "command="], text=True)
        self.assertFalse(
            token in commands, "supervisor secret must not appear in process arguments"
        )
        self.assertNotIn("token", first)
        self.assertEqual(self.run_cli(*args).returncode, 0)
        self.assertEqual(
            json.loads((self.root / "state folder/runtime.json").read_text())["token"],
            token,
        )
        self.assertTrue(json.loads(self.run_cli("status").stdout)["ready"])
        self.assertEqual(self.run_cli("stop").returncode, 0)
        self.assertFalse(json.loads(self.run_cli("status").stdout)["ready"])
        with socket.socket() as s:
            s.bind(("127.0.0.1", 0))
            s.listen()
            port = s.getsockname()[1]
            args = (
                "start",
                "--binary",
                str(fake),
                "--port",
                str(port),
                "--timeout",
                "30",
            )
            r = self.run_cli(*args)
            self.assertNotEqual(r.returncode, 0)
            self.assertIn("port", r.stdout)


if __name__ == "__main__":
    unittest.main()
