# Native release QA — pending unless explicitly marked

Automated tests cannot establish macOS TCC behavior or compatibility with real editors.
Run these through the built .app, with harmless text and an approved provider key.

- [ ] Microphone prompt identifies Text-to-speech; declining it prevents capture.
- [ ] Accessibility/Input Monitoring denied: actionable menu state, no insertion.
- [ ] Space held 350 ms starts one session; repeats do not duplicate it.
- [ ] Quick Space taps and fast typing rollover preserve normal text.
- [ ] Wispr Flow can continue using Fn without starting this app.
- [ ] Modified-Space shortcuts cancel dictation without breaking the shortcut.
- [ ] Escape while recording and while finishing never inserts later.
- [ ] Recording/processing indicator remains visible in full screen and never takes focus.
- [ ] Short phrase and trailing last word survive release; silence produces no text.
- [ ] Unicode, punctuation and selection replacement work in TextEdit.
- [ ] Browser input/textarea and editor compatibility recorded individually.
- [ ] Switching app/field during recording or processing blocks insertion.
- [ ] Secure/password fields and unsupported AX targets are blocked.
- [ ] Multiline/control characters never submit a form or terminal command automatically.
- [ ] Clipboard is unchanged unless Copy Last Transcript is explicitly chosen.
- [ ] Default mic/Bluetooth changes and unplugging stop or report an error safely.
- [ ] Network disconnect, invalid key and delayed end-of-stream produce bounded errors.
- [ ] 120-second limit, Quit and process exit release the microphone.
- [ ] No audio, keys or transcript content is left in logs or local files.
- [ ] Ad-hoc developer signing / Keychain access checked after a rebuild.

No signed or notarized public release exists. Keep any failed case linked to its GitHub issue.
