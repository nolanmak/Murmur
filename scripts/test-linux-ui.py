"""Exercise the real GTK controls with a synthetic worker, never a microphone/key."""
import importlib.util
from pathlib import Path
import tempfile
import time
import unittest
from unittest.mock import patch

root = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("linux_ui", root / "src/platform/linux_ui.py")
ui = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ui)

class DesktopTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        worker = Path(self.tmp.name) / "worker"
        worker.write_text('''#!/usr/bin/python3
import json,sys
if sys.argv[1] == "linux-devices":
 print(json.dumps([{"serial":"42","name":"Synthetic microphone"}]))
else:
 print(json.dumps({"state":"recording"}),flush=True)
 command=sys.stdin.readline().strip()
 # A late provider result must be discarded after cancellation.
 print(json.dumps({"state":"review","text":"Café fixture 👋"}),flush=True)
''')
        worker.chmod(0o700)
        self.window = ui.Murmur(str(worker))
        self.window.show_all()

    def tearDown(self):
        self.window.destroy()
        self.tmp.cleanup()

    def until(self, predicate):
        end = time.monotonic() + 5
        while not predicate():
            while ui.Gtk.events_pending():
                ui.Gtk.main_iteration_do(False)
            if time.monotonic() >= end:
                self.fail("UI operation timed out")
            time.sleep(.01)

    def test_manual_capture_finish_and_explicit_copy(self):
        self.window.start_button.clicked()
        self.until(lambda: self.window.status.get_text().startswith("Recording"))
        self.assertFalse(self.window.start_button.get_sensitive())
        self.window.stop_button.clicked()
        self.until(lambda: self.window.process is None)
        self.assertTrue(self.window.copy_button.get_sensitive())
        with patch.object(ui.Gtk.Clipboard, "get") as clipboard:
            self.window.copy_button.clicked()
            clipboard.return_value.set_text.assert_called_once_with("Café fixture 👋", -1)
        self.assertIn("check", self.window.status.get_text().lower())

    def test_cancel_discards_late_transcript_and_allows_restart(self):
        self.window.start_button.clicked()
        self.until(lambda: self.window.status.get_text().startswith("Recording"))
        self.window.cancel_button.clicked()
        self.until(lambda: self.window.process is None)
        self.assertFalse(self.window.copy_button.get_sensitive())
        self.assertEqual(self.window.text.get_buffer().get_char_count(), 0)
        self.assertTrue(self.window.start_button.get_sensitive())

if __name__ == "__main__":
    unittest.main()
