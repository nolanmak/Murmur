# RustDesk acceptance audit

Scope: [issue #13](https://github.com/nolanmak/Murmur/issues/13).
Implementation review: [draft PR #14](https://github.com/nolanmak/Murmur/pull/14).
This audit records incomplete work; it is not a release sign-off.

## Phase 0

| Requirement | Evidence and remaining gap |
| --- | --- |
| Client/host versions and supported interfaces | Local client 1.4.6 and matching release source inspected; remote host versions missing. See `remote-design.md`. |
| Actual transfer and field insertion on macOS and Linux | Not verified. No remote field was populated by the app during the attempted test. |
| Session, reconnect, clipboard availability, and receipt observability | AX window/title can be observed; connection generation, remote fields, and receipt are not established. |
| Background-session isolation | Release source broadcasts to eligible sessions. Runtime multi-session test missing. Manual fallback requires user confirmation that other sessions are disconnected. |
| Design decision and revised estimate | Manual fallback and limitations documented. Supported automatic transport and release estimate remain open. |

## User flow and destination handling

| Requirement | Evidence and remaining gap |
| --- | --- |
| Opt-in; preserve local behavior when off | Mode defaults off; native menu toggle tested. Local insertion code still executes only in local mode. Local native regression matrix after this change remains open. |
| Local capture/provider credentials; reviewed final text only | Existing capture/streaming tests pass. Remote mode retains the completed transcript for explicit review. No remote provider/service was added. Full voice-to-remote-field test missing. |
| Review transcript and selected destination before sharing | Real dialog preview visually checked with synthetic text. Positive native selected-window review did not complete through automation. |
| Require active, identified, unambiguous session | Not achieved by the automatic path. Manual fallback observes a local window and requires user confirmation; it cannot verify connection or isolation. |
| Explicit paste profiles | macOS/Linux/Linux-terminal profiles have controller tests and menu instructions. Manual fallback does not dispatch a shortcut. |
| Hotkey not forwarded; no Enter/submit | No remote shortcut or Enter is dispatched by the fallback. It uses menu Start/Stop; physical Control events remain unsuppressed. Automatic hotkey requirement remains open. |
| Accurate status at every stage | Controller tests distinguish preparing, waiting, requested, sent, inserted, failed, cancelled. UI reports manual copy only; automatic lifecycle is not wired. |
| Revalidate session/generation/window before share and dispatch | Controller tests cover changed tokens. Manual path revalidates retained AX window/title. Native session/reconnect identity and automatic dispatch are unresolved. |
| Remote widget/password limitations and explicit confirmation | Limitation shown in review and docs. Confirmation is enforced in review tests. No remote widget detection claimed. |
| Stale/double callback safety and explicit retry | Controller, review, and clipboard tests pass. No real asynchronous transport exists yet. |
| Configurable bounded sync/dispatch and cancellation | Controller deadline boundary tests pass. Native promised clipboard reads are not cancellable; automatic adapter deadlines are unimplemented. |
| Distinct disabled-sync/keyboard/locked/disconnected/unsupported errors | Pure controller errors tested. Manual fallback cannot distinguish remote conditions; native classifications remain open. |

## Clipboard, text, and privacy

| Requirement | Evidence and remaining gap |
| --- | --- |
| Unicode/punctuation/whitespace; no command interpretation | Exact Unicode payload tested with fake and real isolated pasteboards. Remote end-to-end integrity unverified. |
| Reject multiline/control characters | Shared validation covered by controller, lease, and review tests. |
| Lossless supported snapshot; reject unsupported/multiple items | Fake tests and four native clipboard contracts pass, including multi-item rejection and rich-text restoration. General clipboard matrix beyond tested formats remains open. |
| Restore only after consumption or explicit action; preserve newer owner | Lease tests pass for receipt-less retention, ownership changes, partial failures, stale cleanup, and explicit restore. NSPasteboard has a non-atomic check/write race; absolute ownership guarantee unresolved. |
| Explain remote clipboard effects | Review and restore dialogs and setup docs explain synchronization and do not promise remote restoration. |
| No payload/secrets/addresses/names in logs or Git; synthetic CI | Payload types lack Debug; native diagnostics contain booleans/error codes only. Full-history gitleaks and personal-path checks passed; credentials remain ignored. Live window titles are displayed in review only, not written to diagnostics or Git. |

## Architecture and TDD

| Requirement | Evidence and remaining gap |
| --- | --- |
| Pure controller; lifecycle separate from callbacks | `remote.rs` and `remote_review.rs` are independent of native APIs. |
| Injectable session, clipboard transport, dispatcher, clock | Clipboard trait and explicit monotonic-time controller inputs exist. Session/transport/dispatcher adapter contracts await the transport decision. |
| Separate from local insertion/provider | Separate modules and opt-in UI branch; local provider connection remains unchanged. |
| Attempt-scoped, idempotent cleanup | Lease tests cover duplicates/stale attempts and failures. Shutdown retains copied text without a blind restore and discards in-memory recovery. Native race remains open. |
| Red/green behavioral evidence | Red commits and green results linked in `remote-design.md`: controller 12, clipboard 10, review 6 tests; four native clipboard contracts. |
| Fake and real contracts; chaos cases | Fake lifecycle tests and real private-pasteboard contracts exist. Actual RustDesk transport contracts, delayed sync, reconnect and disconnect tests missing. |
| Test app insertion path, not test-runner paste | `run --remote-fixture` feeds the real review/copy path. Attempt blocked before copying, so no remote insertion pass is claimed. |

## Native matrix

| Target | Status | Evidence needed |
| --- | --- | --- |
| macOS Notes/text editor | Not run to completion | Prepared disposable editor, successful app copy/manual paste, exact contents and surrounding text. |
| macOS Chrome input/textarea/contenteditable | Not run | Exact contents in all three inputs; no form submitted. |
| macOS Terminal | Not run for RustDesk | Correct paste into a harmless prompt; no execution. Prior local terminal success is not remote evidence. |
| Linux/X11 browser and terminal | Environment not prepared | Host/version information and exact contents with both paste profiles. |
| Linux/Wayland browser and terminal | Environment not prepared | Supported configuration result, or documented unsupported behavior that blocks gracefully. |
| Slow/disabled sync, focus/session changes, disconnect | Controller-only tests | Native no-wrong-target/no-duplicate/no-late-paste evidence and recoverable transcript. |

## Completion gates

Formatting, Clippy, workspace tests, repository-boundary checks, and full-history
secret scanning passed locally for the implementation. Setup/troubleshooting docs
and a draft PR exist. CI is a separate check and does not prove native insertion.

The feature is **not complete**. Next required external input is a manually
focused disposable macOS test editor and an available Linux desktop test session.
Do not supply credentials or connection IDs in public evidence. The automatic
transport decision and native guarantees remain open independently of those tests.
