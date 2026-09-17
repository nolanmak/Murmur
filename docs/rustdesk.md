# RustDesk dictation: manual review preview

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
- Restore the pending previous clipboard before copying another reviewed
  transcript. If a write partially fails, the recovery snapshot remains available.
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
