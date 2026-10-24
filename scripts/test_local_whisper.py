import hashlib, json, pathlib, socket, subprocess, sys, tempfile, unittest
import importlib.util, io, http.server, threading, os
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location(
    "runtime", pathlib.Path(__file__).with_name("local-whisper.py")
)
runtime = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runtime)
SCRIPT = pathlib.Path(__file__).with_name("local-whisper.py")


class RuntimeTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="local whisper test ")
        self.root = pathlib.Path(self.tmp.name)
        self.model = self.root / "source model.bin"
        self.model.write_bytes(b"fixture model")
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

    def test_loopback_supervisor_startup_never_resolves_a_hostname(self):
        state = self.root / "state folder"
        state.mkdir()
        child = Mock()
        child.poll.return_value = 0
        with patch.object(socket, "getfqdn", side_effect=AssertionError("DNS must not be needed")):
            with patch.object(runtime.subprocess, "Popen", return_value=child):
                runtime.supervise(state, "fixture-engine", 17863, "a" * 64, 0.5)
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
