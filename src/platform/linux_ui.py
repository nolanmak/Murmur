"""Linux manual dictation shell; all capture and provider work stays in Rust."""
import json
import os
import signal
import subprocess
import sys
import threading

import gi
gi.require_version("Gtk", "3.0")
gi.require_version("Gdk", "3.0")
from gi.repository import Gdk, GLib, Gtk


class Murmur(Gtk.Window):
    def __init__(self, executable):
        super().__init__(title="Murmur — Linux preview")
        self.executable = executable
        self.process = None
        self.cancelled = False
        self.closing = False
        self.set_default_size(480, 340)
        self.set_border_width(18)
        self.connect("delete-event", self.close)
        self.connect("key-press-event", self.key)
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12)
        self.add(box)
        self.status = Gtk.Label(label="Ready — select a microphone and start.")
        self.status.set_line_wrap(True)
        box.pack_start(self.status, False, False, 0)
        self.devices = Gtk.ComboBoxText()
        box.pack_start(self.devices, False, False, 0)
        controls = Gtk.Box(spacing=8)
        box.pack_start(controls, False, False, 0)
        self.start_button = Gtk.Button(label="Start dictation")
        self.stop_button = Gtk.Button(label="Stop")
        self.cancel_button = Gtk.Button(label="Cancel")
        self.refresh_button = Gtk.Button(label="Refresh inputs")
        for button, callback in [(self.start_button, self.start), (self.stop_button, self.stop), (self.cancel_button, self.cancel), (self.refresh_button, self.refresh)]:
            button.connect("clicked", callback)
            controls.pack_start(button, True, True, 0)
        self.text = Gtk.TextView()
        self.text.set_editable(False)
        self.text.set_wrap_mode(Gtk.WrapMode.WORD_CHAR)
        scroll = Gtk.ScrolledWindow()
        scroll.add(self.text)
        box.pack_start(scroll, True, True, 0)
        self.copy_button = Gtk.Button(label="Copy transcript")
        self.copy_button.connect("clicked", self.copy)
        box.pack_start(self.copy_button, False, False, 0)
        note = Gtk.Label(label="Paste into your chosen field: Ctrl+V, or Ctrl+Shift+V in a terminal.\nRustDesk: audio must be available as an input on this computer.\nCopy may sync to connected RustDesk clients. No automatic paste.")
        note.set_line_wrap(True)
        box.pack_start(note, False, False, 0)
        self.copy_button.set_sensitive(False)
        self.busy(False)
        self.refresh()

    def busy(self, active):
        self.start_button.set_sensitive(not active and self.devices.get_active_id() is not None)
        self.devices.set_sensitive(not active)
        self.refresh_button.set_sensitive(not active)
        self.stop_button.set_sensitive(active)
        self.cancel_button.set_sensitive(active)

    def refresh(self, *_):
        self.devices.remove_all()
        try:
            result = subprocess.run([self.executable, "linux-devices"], capture_output=True, text=True, timeout=5)
            if result.returncode:
                raise RuntimeError(result.stderr.strip())
            for mic in json.loads(result.stdout):
                self.devices.append(mic["serial"], mic["name"])
            self.devices.set_active(0)
            if self.devices.get_active_id() is None:
                self.status.set_text("No microphone input. Connect one to this computer, then refresh.")
        except (OSError, ValueError, RuntimeError, subprocess.TimeoutExpired) as error:
            self.status.set_text(str(error))
        self.busy(False)

    def start(self, *_):
        if self.process is not None or self.devices.get_active_id() is None:
            return
        self.cancelled = False
        self.copy_button.set_sensitive(False)
        self.text.get_buffer().set_text("")
        try:
            self.process = subprocess.Popen([self.executable, "linux-session", self.devices.get_active_id()], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, bufsize=1, start_new_session=True)
        except OSError:
            self.status.set_text("Cannot start dictation worker.")
            return
        self.status.set_text("Starting microphone…")
        self.busy(True)
        threading.Thread(target=self.read_worker, args=(self.process,), daemon=True).start()

    def read_worker(self, process):
        for line in process.stdout:
            try:
                event = json.loads(line)
            except ValueError:
                continue
            GLib.idle_add(self.event, process, event)
        error = process.stderr.read().strip()
        process.wait()
        GLib.idle_add(self.finished, process, error)

    def event(self, process, event):
        if process is not self.process or self.cancelled:
            return False
        state = event.get("state")
        if state == "recording":
            if self.stop_button.get_sensitive():
                self.status.set_text("Recording — speak, then click Stop. Esc cancels here.")
        elif state == "review":
            text = event.get("text", "")
            self.text.get_buffer().set_text(text)
            self.copy_button.set_sensitive(bool(text))
            self.status.set_text("Review your transcript, then Copy and paste." if text else "No speech detected.")
        elif state == "error":
            self.status.set_text(event.get("message", "Dictation failed."))
        elif state == "cancelled":
            self.status.set_text("Cancelled.")
        return False

    def finished(self, process, error):
        if process is self.process:
            if error and not self.cancelled:
                self.status.set_text(error)
            process.stdin.close()
            process.stdout.close()
            process.stderr.close()
            self.process = None
            self.busy(False)
            if self.closing:
                Gtk.main_quit()
        return False

    def send(self, command):
        if self.process is not None:
            try:
                self.process.stdin.write(command + "\n")
                self.process.stdin.flush()
            except (BrokenPipeError, OSError):
                pass

    def stop(self, *_):
        if self.process is not None:
            self.send("stop")
            self.status.set_text("Processing…")
            self.stop_button.set_sensitive(False)

    def cancel(self, *_):
        if self.process is not None:
            self.cancelled = True
            self.send("cancel")
            self.status.set_text("Cancelled — closing microphone…")
            self.stop_button.set_sensitive(False)
            self.cancel_button.set_sensitive(False)
            self.copy_button.set_sensitive(False)
            self.text.get_buffer().set_text("")

    def key(self, _, event):
        if event.keyval == Gdk.KEY_Escape:
            self.cancel()
            return True
        return False

    def copy(self, *_):
        buffer = self.text.get_buffer()
        text = buffer.get_text(buffer.get_start_iter(), buffer.get_end_iter(), False)
        if text:
            Gtk.Clipboard.get(Gdk.SELECTION_CLIPBOARD).set_text(text, -1)
            self.status.set_text("Copied — focus your chosen field and paste. Check the result.")

    def close(self, *_):
        if self.process is not None:
            self.closing = True
            self.cancel()
            self.hide()
            GLib.timeout_add_seconds(4, self.force_close)
        else:
            Gtk.main_quit()
        return True

    def force_close(self):
        if self.process is not None:
            os.killpg(self.process.pid, signal.SIGKILL)
        Gtk.main_quit()
        return False


if __name__ == "__main__":
    if not Gtk.init_check()[0]:
        sys.exit("No graphical display. Open Murmur in your desktop session.")
    window = Murmur(sys.argv[1])
    window.show_all()
    Gtk.main()
