# RustDesk dictation: manual review preview

## Automatic recording in a focused RustDesk window

Hold Control with the intended RustDesk remote window focused to record on the
Mac; release to finish and copy the fresh transcript. No mode toggle is needed.
Focus is checked again before copying. Paste manually into the remote terminal
with its normal shortcut. Murmur cannot observe the remote text field or confirm
delivery. Clipboard sync may share text with other connected sessions, so keep
only the intended session connected. The Control modifier still reaches RustDesk.
Local browser and terminal fields continue using local insertion automatically.
The manual review controls below remain available as an alternative.

This development feature keeps speech capture and provider credentials on the
local Mac. **Automatic remote paste is not available yet.** The current fallback
copies reviewed text; you perform the paste in RustDesk yourself.

## Use the fallback

1. Open the **Control** menu in the macOS menu bar and enable **Remote review
   mode**. The compact floating pill is click-through; controls stay in the menu.
2. Use **Start dictation** and **Stop dictation** in the menu. Remote mode does not
   use the Control dictation hotkey, since the current listener cannot suppress
   modifier events sent to RustDesk. Ordinary keyboard events still reach the
   remote computer; use the menu for recording.
3. Choose **Remote paste shortcut** to cycle through macOS `⌘V`, Linux `Ctrl+V`,
   and Linux terminal `Ctrl+Shift+V`. This is an instruction for your manual paste;
   the app does not dispatch that shortcut.
4. Disconnect other RustDesk sessions. Select the intended remote window, then
   choose **Review for RustDesk…** from the Control menu.
5. Read the transcript and selected window, confirm the intended session/field,
   and choose **Copy for manual paste**. Clipboard sync must be enabled in RustDesk.
6. Focus the intended remote text field and paste using the selected shortcut.
   Check the actual field contents. The app cannot confirm remote receipt or
   detect a remote password field. It never sends Enter or executes the text.
7. After pasting, optionally choose **Restore previous clipboard…**. A newer copy
   is kept. Recovery exists only in memory until this app quits. Clipboard sync
   can propagate both the transcript and restored content to the remote host.

Turn remote review mode off to resume local hold-Control dictation. The mode and
paste profile reset on app restart; no transcript or destination is saved.

## Recovery and limits

- If the window changes during review, copying is blocked. Select the intended
  remote window and review again. The window check cannot prove that a connection
  remained active or that RustDesk did not reconnect inside the same window.
- If confirmation is unchecked, nothing is copied. Open review again to confirm.
- If a clipboard cannot be preserved, the transcript remains available. Copy
  simple text locally, then retry. Multiple items, unknown formats, and large or
  unavailable clipboard representations are rejected instead of discarded.
- A new explicitly reviewed copy can replace the previous successful copy,
  retaining the original clipboard snapshot for optional restoration. Finish
  pasting the previous transcript before reviewing the next one. Failed writes
  still require recovery. If another app or user copy has
  replaced it, Murmur detects that revision on the next review and releases
  the stale recovery lease without overwriting the newer copy. If a write
  partially fails, the recovery snapshot remains available.
- A local copy is not evidence of remote delivery. Clipboard permissions, a locked
  host, a disconnected session, or unsupported host configuration may prevent
  transfer; this fallback cannot distinguish them automatically.
- macOS clipboard operations are not atomic. See the [design limitations](remote-design.md)
  before treating this as a release-ready remote transport.
- The macOS/Linux remote acceptance matrix is still unverified. Headless Linux
  servers are outside this graphical RustDesk workflow; a local SSH terminal uses
  the existing local dictation path.

## UI-only smoke preview

`cargo run --locked -- review-preview` opens the actual review dialog using
synthetic text. It never reads credentials, records audio, or modifies a clipboard.
This checks the local review layout; it does not test remote insertion.

For a native integration test, launch the built app with `run --remote-fixture`.
It starts in remote review mode with a labeled synthetic transcript. Unlike the
UI-only preview, its explicit **Copy for manual paste** action uses the real
clipboard. Prepare a disposable remote editor and isolate the intended session
before confirming. Restart normally after testing to clear the fixture.

## Mac → Linux setup

Run Murmur **on the Mac whose microphone you use**, alongside the RustDesk client.
The Linux host needs a graphical RustDesk session with clipboard and keyboard
permissions. It does not need Murmur or a Deepgram key for this workflow. Audio
travels from the Mac directly to Deepgram; only the explicitly copied transcript
travels through RustDesk. Native Linux dictation is a separate feature using a
microphone attached to the Linux host.

Each successful macOS CI run now offers a `Murmur-macOS-preview` artifact with an
ad-hoc-signed app archive and SHA-256 checksum. Download it from the corresponding
GitHub Actions run, verify the checksum, extract, and launch Murmur.app. This is a
developer preview, not a notarized release. Configure credentials on the Mac using
the README, then grant Microphone and Accessibility permissions to that build.
Do not place credentials inside the app.

For a Linux text editor select the Linux Ctrl+V profile; for a Linux terminal
select Linux terminal Ctrl+Shift+V. Use Start/Stop in Murmur's Mac menu, select the
RustDesk session, review and copy, then focus the intended Linux field and paste.
The copy confirmation also confirms other RustDesk sessions are disconnected.
Check the field contents yourself before pressing Enter.

The RustDesk observer recognizes the legacy `com.carriez.rustdesk` identity and
`com.carriez.flutterHbb`, declared in the official 1.4.6 macOS build configuration.
It still requires a supported remote-window title, and revalidates the retained
window before copying. Unknown/custom bundle identities fail closed.
