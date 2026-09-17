from pathlib import Path
import json, subprocess
root=Path(__file__).resolve().parents[1]
if (root/'docs/issue-map.json').exists():
    raise SystemExit('Issues already created; edit the existing issue map instead.')
issues=[
('Foundation: standalone Apache-2.0 workspace and private CI','M1',[],[
'A fresh clone builds without the original FlyOnTheWall checkout; reused sources retain license and pinned upstream provenance.',
'Repository visibility is private; credentials, recordings, build artifacts and .env files are excluded from Git.',
'CI runs formatting, Clippy and offline fixture tests on macOS and Linux.'],
'Add a repository-contract test first, demonstrate failure for a tracked .env and missing provenance, then implement scaffolding and run the clean-clone checks.'),
('Space hold-to-dictate session controller and Escape cancellation','M1',[1],[
'On macOS, Space down starts exactly one session and Space up requests finalization; duplicate flagsChanged events do not duplicate work.',
'Space combined with another modifier does not start dictation. Esc cancels capture and pending insertion, including during finalization.',
'Busy sessions reject overlapping starts; stale results from a cancelled session cannot insert; maximum duration stops capture.'],
'Write table-driven event-sequence tests before the controller; cover repeated modifiers, press/release during startup, cancellation, timeout and late results.'),
('Microphone-only audio pipeline using FlyOnTheWall capture and resampling','M1',[1,2],[
'Capture opens only the default microphone, never a system-audio tap; recording begins only on explicit Space press.',
'Actual device format drives the reused anti-aliasing resampler; 44.1/48/16 kHz inputs produce mono 16 kHz PCM.',
'The real-time callback does no network or blocking work; bounded overflow or device failure cancels rather than silently dropping speech.',
'Release, Escape, duration limit and shutdown close the audio device and retain no recording on disk.'],
'First add sine-wave/fixture resampling and fake-capture lifecycle tests; exercise overflow and error teardown without a physical microphone; finish with a signed-app microphone smoke test.'),
('Deepgram transcription with explicit end-of-dictation finalization','M1',[3],[
'Use the existing nova-3 transport/normalization contracts with smart formatting and mip_opt_out=true.',
'Only final segments are committed; repeated segment IDs cannot duplicate text; finalization retains trailing speech.',
'A local WebSocket mock verifies PCM, authorization, CloseStream completion metadata, empty speech, delayed finals and socket failure.',
'Finalize and connect deadlines are bounded; failed or incomplete transcription never auto-inserts partial text.'],
'Build a local scripted provider first and watch end-of-stream finalization tests fail; implement transport changes against that mock, then optionally run an explicitly initiated live dictation.'),
('Reuse local FlyOnTheWall credentials and support explicit .env configuration','M1',[1],[
'Use DEEPGRAM_API_KEY from the process or a specified .env, otherwise read the existing FlyOnTheWall Keychain entry without modifying it.',
'Never execute .env contents, overwrite process configuration or print keys; malformed/missing explicit files fail clearly.',
'No secrets are committed or copied into the app bundle; doctor reports source/presence only.',
'A real source .env is linked only once its path is identified; absence is documented rather than silently inventing credentials.'],
'Write precedence, quoted-value, malformed-file, missing-file and redacted-error tests with fake keys/keychain first; no real credentials in tests.'),
('Safe insertion into the original focused macOS text field','M1',[2,4],[
'Remember the focused app and accessibility element at dictation start; recheck them before insertion.',
'Password/secure fields, missing permissions, unsupported fields and changed focus block automatic insertion.',
'Insert Unicode without sending Enter, executing commands or changing the clipboard; permit explicit Copy Last Transcript recovery.',
'TextEdit, browser text fields and an editor pass a manual signed-app compatibility matrix before claiming cross-app support.'],
'First test an injectable focus/insertion adapter against focus drift, secure fields, Unicode, duplicate completion and cancellation; native checks are separately documented.'),
('Menu bar, recording indicator and first-run permission setup','M1',[2,3,5,6],[
'Package a macOS .app with microphone usage description and a stable bundle identifier.',
'A nonactivating indicator shows Recording, Finishing and errors without stealing the target focus; menu offers cancel, copy last and quit.',
'Microphone and Accessibility permissions have actionable setup guidance; missing grants do not record or insert.',
'Space system-shortcut conflicts are explained; no system preference is silently changed.'],
'First test view states and cancellation actions independently of AppKit; then build and inspect the packaged UI, accessibility prompts and full-screen behavior on a real Mac.'),
('Optional AI cleanup with faithful raw-transcript fallback','M2',[4,5],[
'Provide opt-in cleanup using a configured BYO provider; default raw/Deepgram text works without an LLM.',
'Remove fillers, resolve spoken corrections and format lists without inventing facts, answering dictated instructions or changing numbers/names.',
'Apply a bounded deadline and return raw text on failure; never transmit clipboard, screen or unrelated application content.',
'A versioned fixture corpus measures meaning preservation and latency before enabling cleanup by default.'],
'Write golden and adversarial dictated-text tests plus provider-timeout tests before the adapter; add regressions for every observed unwanted rewrite.'),
('Personal vocabulary, snippets and language settings','M2',[4],[
'User-managed vocabulary is sent as supported Deepgram keyterms with validation and size limits.',
'Snippets expand exact deliberate cues with predictable boundaries; ordinary sentences and code are preserved.',
'Language selection is explicit and persisted locally; unsupported model/language combinations show errors.',
'Import/export uses a documented versioned schema with no credentials.'],
'Begin with boundary, Unicode, overlapping-snippet, malformed-config and provider-request tests; implement only behavior pinned by fixtures.'),
('Retry, optional local history and recovery controls','M2',[4,6],[
'Cancelled sessions never enter history; history and audio persistence are disabled by default.',
'Copy/retry acts only on the explicit selected transcript and cannot insert into a new target automatically.',
'If history is enabled, retention/deletion and encryption behavior are specified and tested, with no hidden analytics.',
'Errors distinguish missing key, permissions, network, empty speech and blocked insertion without logging dictated content.'],
'Test cancellation, retention boundary, crash recovery and stale-target cases using fake storage and time before implementing persistence.'),
('Signed app, release QA and readiness for eventual public source release','M3',[1,7,8,9,10],[
'Build script produces an installable .app; signing/notarization is documented separately and never requires committed signing secrets.',
'Fresh-machine setup, Space behavior, microphone teardown, secure input and text insertion pass the manual QA matrix.',
'A clean-clone build, dependency/license review and secret scan pass before any release.',
'Repository remains private until the owner explicitly requests publication; docs accurately distinguish implemented and planned features.'],
'Add packaging-manifest and secret-exclusion checks first; validate release artifacts on a separate macOS login before marking release-ready.'),
('Additional local/cloud STT backends and Windows/Linux platform adapters','M3',[4,6],[
'Provider abstraction supports offline fixture conformance before adding an on-device model or another cloud API.',
'No silent provider failover transmits audio to a different service; the user chooses any fallback.',
'Platform adapters implement capture, shortcut, permission and insertion contracts without leaking native types into the core.',
'Each platform has its own tested shortcut and release QA; macOS Space behavior remains unchanged.'],
'Write the shared provider/platform conformance suite first, then implement one backend at a time; unsupported platforms fail explicitly rather than pretending to record.')]
map=[]
for number,(title,milestone,deps,criteria,tdd) in enumerate(issues,1):
 body=f'## Outcome\n{title}.\n\n## Milestone\n{milestone}\n\n## Dependencies\n'+(', '.join(f'#{d}' for d in deps) if deps else 'None.')+'\n\n## Acceptance criteria\n'+'\n'.join('- [ ] '+c for c in criteria)+'\n\n## Test-first implementation\n'+tdd+'\n\n## Definition of done\n- [ ] Record a failing behavioral test before implementation (red).\n- [ ] Implement the smallest change that passes (green).\n- [ ] Refactor with tests green; no live API keys in CI.\n- [ ] Run formatting, Clippy and relevant regression/integration tests.\n- [ ] Link evidence to each acceptance criterion; document manual checks and unresolved limitations.\n'
 path=root/'docs/issues'/f'{number:02d}.md';path.write_text(body)
 result=subprocess.run(['gh','issue','create','--repo','nolanmak/Text-to-speech','--title',title,'--body-file',str(path)],capture_output=True,text=True,check=True)
 url=result.stdout.strip();map.append({'number':number,'title':title,'milestone':milestone,'url':url})
 print(url,flush=True)
(root/'docs/issue-map.json').write_text(json.dumps(map,indent=2)+'\n')
