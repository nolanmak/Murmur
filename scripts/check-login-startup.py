#!/usr/bin/env python3
"""Native smoke check of real app startup with an isolated HOME; no reboot proof.

Run only in an interactive macOS session. Do not press dictation shortcuts while
this runs. The test starts the bundle executable normally, without a fixture mode,
and terminates only its own child. It never bootstraps a launchd service.
"""
import os
from pathlib import Path
import plistlib
import subprocess
import sys
import tempfile
import time

bundle = Path(sys.argv[1]).resolve()
executable = bundle / "Contents/MacOS/murmur"
assert executable.is_file(), executable
with tempfile.TemporaryDirectory(prefix="murmur-login-qa-") as directory:
    home = Path(directory)
    agent = home / "Library/LaunchAgents/com.shipsystems.texttospeech.login.plist"
    with tempfile.TemporaryFile() as log:
        process = subprocess.Popen([str(executable)], env={**os.environ, "HOME": directory},
                                   stdout=log, stderr=log)
        try:
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline and not agent.exists():
                assert process.poll() is None, "app exited before registering startup"
                time.sleep(0.1)
            assert process.poll() is None, "app is not running"
            assert agent.is_file(), "healthy app did not create its login registration within 10 seconds"
            with agent.open("rb") as stream:
                registration = plistlib.load(stream)
            assert registration["ProgramArguments"] == ["/usr/bin/open", str(bundle)]
            assert registration["RunAtLoad"] is True
            assert registration["Label"] == "com.shipsystems.texttospeech.login"
            assert list(agent.parent.glob("*.plist")) == [agent]
            time.sleep(1)
            assert process.poll() is None, "app exited after registering startup"
            log.seek(0)
            output = log.read().decode(errors="replace")
            assert "permissions accessibility=" in output, "native shell did not finish initialization"
            assert "Starting microphone" not in output
            print("PASS: native shell alive; app created one valid registration in isolated HOME; no capture start")
        finally:
            if process.poll() is None:
                process.terminate()
            process.wait(timeout=10)
