# Linux developer preview

Murmur now has a native Linux **manual dictation and review/copy** path. It is an
experimental slice of issues #16, #17, #21, #22 and #23, not completion of the
Linux epic. Hold-Control (#18), automatic focused insertion (#19), and a
validated Wayland portal adapter (#20) are not implemented.

## Tested target

| Component | Target / evidence |
| --- | --- |
| OS / architecture | Ubuntu 24.04.2 LTS, x86_64 |
| Desktop / session | GNOME 46.0, X11 |
| Audio | PipeWire 1.0.5, `pw-record` raw f32 mono 48 kHz |
| UI | GTK 3.24.41 via system Python GObject |
| Rust | 1.95.0 |
| Native automation | Isolated PipeWire sine source → real adapter → existing 16 kHz converter → local fake WebSocket → final transcript; stop and cancellation release the input |
| Desktop controls | Real GTK Start/Stop/Cancel/review with a synthetic worker; explicit copy payload verified without modifying the user's clipboard |
| Hardware microphone / live speech | Input enumerated; real speech-to-Deepgram acceptance remains a manual gate |
| Wayland | Unverified; no global shortcut or automatic insertion claimed |
| Headless | Desktop launch fails clearly and does not start capture |

## Install and use

Build prerequisites on Ubuntu 24.04:

```sh
sudo apt install build-essential pkg-config libssl-dev libdbus-1-dev \
  pipewire-bin python3-gi gir1.2-gtk-3.0
./scripts/install-linux.sh
```

The installer builds a release binary with the pinned Rust toolchain and installs
it to `~/.local/bin/murmur`, plus an application launcher, LICENSE and NOTICE.
It needs no root privileges itself, installs no service, and bundles no keys.
Use your desktop application menu to launch **Murmur**, select an input, click
**Start dictation**, speak when Recording appears, and click **Stop**. Review the
result, explicitly **Copy transcript**, then paste into your chosen input. Use
Ctrl+V for ordinary fields or Ctrl+Shift+V for terminals. Nothing sends Enter.
Esc cancels while the Murmur window is focused; the Cancel button is always
available during capture/processing. There is no global key listener in this
preview. The regular window is the control surface; tray support is not required.

Audio/transcripts stay in memory; copies may be retained by your desktop clipboard
manager. Copy intentionally replaces the clipboard and may synchronize through
RustDesk. No clipboard snapshot/restoration is claimed on Linux. Closing Murmur
cancels capture; a recording has the existing 120-second duration limit, bounded
queues, three-second no-audio watchdog and bounded provider finalization.

PipeWire performs negotiated conversion to raw float32 mono 48 kHz; Murmur uses
its existing resampler for Deepgram's mono 16 kHz stream. The chosen source serial
is validated before capture; sinks and named monitor sources are excluded.
Capture is pinned to that source and does not reconnect to another source.
After unplug/default changes, refresh inputs and explicitly select the device.

## Credentials

Resolution order:

1. Nonempty process `DEEPGRAM_API_KEY`.
2. Explicit `FOTW_ENV_FILE`, if supplied. Missing/malformed files fail.
3. `$XDG_CONFIG_HOME/murmur/env` (absolute XDG path only), otherwise
   `~/.config/murmur/env`.

The file uses the `.env.example` schema and must have private permissions (600).
Contents are parsed, never executed; values are never included in error messages.
Process configuration overrides the file. `murmur doctor` reports source only.
Linux does not search a macOS Keychain or silently load a working-directory file.

To remove the installed app while retaining credentials:

```sh
python3 scripts/install-linux.py uninstall
```

Optional login startup: use your desktop's Startup Applications settings to run
`~/.local/bin/murmur`. This is not enabled by installation.

## RustDesk: where the microphone lives

If you sit at a **Mac** and remotely control Linux, run Murmur on the **Mac**.
Use [RustDesk review/copy](rustdesk.md) to transfer the resulting text. Installing
this Linux preview does not forward your Mac microphone. A remote/headless
machine can capture only input sources actually available to its audio service.
No remote provider key or Linux Murmur installation is required for Mac dictation.

## Verification

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --workspace
python3 scripts/check-repo.py
python3 scripts/test-linux-ui.py      # graphical session, or xvfb-run -a
python3 scripts/test-linux-native.py  # isolated server, synthetic source only
```

The native fixture runner requires `pipewire-bin` and `libspa-0.2-modules`. It
starts its own temporary PipeWire server; it does not attach to or reconfigure
your real audio graph. No hardware audio, real provider credentials, or public
network is used. Its two tests are explicitly ignored in ordinary workspace
runs and run separately in Linux CI. This is not evidence of a live microphone
or RustDesk transfer. No Linux child issue is marked complete by this preview.

Test-first evidence: the initial `cargo test --locked --test linux` failed because
the Linux adapter did not exist. The implemented tests pass for microphone
filtering, config precedence, file permissions, redacted errors and headless
startup. Native GTK tests reproduced a Gdk 4 / Gtk 3 namespace conflict; explicitly
pinning both namespaces to 3.0 resolved it. The baseline workspace suite passed
before changes. A temporary-home install/uninstall check preserved configuration.
