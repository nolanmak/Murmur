# RustDesk remote dictation — implementation evidence

Issue: [#13](https://github.com/nolanmak/Murmur/issues/13).

## Transport investigation

The inspected local RustDesk installation reports version **1.4.6**. No remote
host versions or native insertion results have been collected yet.

Upstream source inspected at revision
`fd0fe592eb78f2326c3ea53e6a0f383f8a46b882`:

- [Client clipboard loop](https://github.com/rustdesk/rustdesk/blob/fd0fe592eb78f2326c3ea53e6a0f383f8a46b882/src/client.rs):
  the comments on `try_start_clipboard` explicitly describe sending detected text
  clipboard updates to all sessions. A foreground window alone therefore does
  not establish destination isolation.
- [CLI entry point](https://github.com/rustdesk/rustdesk/blob/fd0fe592eb78f2326c3ea53e6a0f383f8a46b882/src/core_main.rs):
  this inspection did not identify a supported CLI for querying clipboard receipt
  or verifying insertion into a remote field. This is a limited source review,
  not proof that every RustDesk integration lacks such an interface.
- [Documented settings](https://rustdesk.com/docs/en/self-host/client-configuration/advanced-settings/)
  describe clipboard and keyboard permissions. These settings alone do not
  establish that a particular transcript was consumed.

The initial source revision above was a research reference. A follow-up review
checked the **1.4.6 release tag**, revision
`1abc897c451c8b5bbff3792509a7fef9d12f2ce3`:

- [Release clipboard dispatch](https://github.com/rustdesk/rustdesk/blob/1abc897c451c8b5bbff3792509a7fef9d12f2ce3/src/flutter.rs#L1451)
  iterates all sessions and sends text clipboard messages to every session whose
  `is_text_clipboard_required` condition is true. There is no foreground-window
  filter in this function. This confirms that choosing a local window alone is
  insufficient to isolate the destination.
- [Release CLI](https://github.com/rustdesk/rustdesk/blob/1abc897c451c8b5bbff3792509a7fef9d12f2ce3/src/core_main.rs)
  exposes connection and management commands. The inspected dispatch contains no
  command for querying remote text insertion or clipboard receipt.

These are source observations, not a runtime multi-session test or a guarantee
about every available RustDesk integration. Remote-host versions, permissions,
and actual transfer behavior still need validation.

## Controller implemented

`src/remote.rs` contains an independent state machine. It accepts only opaque,
ephemeral destination identifiers and synthetic monotonic elapsed times; it
stores no transcript, clipboard payload, remote address, or machine name.
The caller retains the review text for recovery.

Explicit opt-in and valid single-line text are required before an attempt begins.
Session/window/reconnection identity and permission state are checked before
sharing and again before issuing a paste command. Duplicate and stale callbacks
cannot issue a second command. Deadlines expire at their exact boundary.
The command represents only the selected paste chord, without Return or Enter.

Adapters must supply trustworthy observations. Unknown state must not be filled
in as `true`. Clipboard receipt must come from transport evidence, not a timer.
The clipboard-sharing authorization and returned paste command must each be
consumed synchronously with their fresh destination check; queuing a command for
later execution would require another check. Native callbacks should enqueue
observations for the main-thread controller rather than run blocking operations.

The automatic controller is not connected to a RustDesk transport yet. A separate
explicit manual-copy fallback is now wired into the menu and transcript completion
path. See [setup and limitations](rustdesk.md). Local insertion remains the default.

The installed RustDesk accessibility tree exposes the remote window as a single
container; the inspected tree did not expose remote fields or delivery status.
The fallback retains an AX window reference and compares the window/title before
copying. It does not treat that comparison as connection or receipt verification.
The user explicitly confirms session isolation and the intended field. No real
remote content, address, or window title is stored in repository evidence.

## Clipboard implementation and limits

`remote_clipboard::Lease` keeps recovery data only in memory, scoped to an attempt.
It preserves a partial-write recovery snapshot, rejects stale cleanup, and retains
the copied transcript when receipt is unknown. Explicit restoration checks the
clipboard revision. A changed owner ends recovery without overwriting that owner.
There is no timer-based restoration or implicit write in `Drop`.

`platform::macos_clipboard::MacClipboard` implements the same clipboard interface
using NSPasteboard. It preserves materialized supported text/rich-text/image
formats, including observed macOS text aliases; it rejects multiple items,
unknown formats, failed materialization, and snapshots exceeding 16 MiB. It
requires the main thread because macOS may synchronously fulfill format promises.

**Native limitation:** NSPasteboard does not provide atomic compare-and-replace.
The adapter checks change counts immediately before mutation, between format
writes, and afterward, but another process can race the check and declaration.
The fake adapter's atomic conflict tests do not prove the absence of that native
race. A native design that satisfies the issue's absolute ownership guarantee is
still unresolved; do not enable unattended remote clipboard automation on this
evidence. Reading promised data also lacks a cancellable native deadline.

No local or remote general clipboard was modified during the native adapter
tests. Each test creates and clears its own uniquely named pasteboard.

## TDD evidence

Command: `cargo test --locked --test remote`.

- Red commit `8395a31`: 12 behavioral tests failed against the compiling controller
  stub, including explicit sharing, session drift, cancellation, timeout bounds,
  and paste profiles.
- Green implementation: the same 12 tests pass without weakening assertions.
- These are controller tests. No native RustDesk acceptance row is marked passed.
- Clipboard red commit `f890fa9`: all 10 clipboard lifecycle tests failed against
  the compiling stub. Green commit `ba5d56f`: those tests pass, including exact
  Unicode payloads, multi-format restoration, stale attempts, partial copy/restore
  failures, ownership changes, and explicit recovery without receipt.
- Native adapter red commit `f78c3ac`: four real NSPasteboard contracts failed
  against the compiling stub. The implementation passes all four via
  `cargo test --locked --test macos_clipboard`: Unicode/rich-text round-trip,
  newer-owner preservation, multiple-item rejection, and empty restoration.
  Its custom harness runs on the main thread. Non-macOS runs explicitly skip.
  This proves local clipboard behavior, not transport delivery or remote paste.
- Review red commit `f8cb77e`: four compiling behavioral tests failed. The same
  tests pass with explicit confirmation, one-time text release, window drift,
  replacement review, cancellation, and invalid-input handling implemented.
- The actual review dialog was launched with `review-preview` and inspected both
  through accessibility and a window-only screenshot. Synthetic Unicode text,
  scroll area, unchecked confirmation, selected profile, copy, and cancel controls
  were visible without clipping. No clipboard sharing or remote insertion occurred.
- Focus-transition red commit `35fea99`: two new review integration tests failed
  while four prior review tests passed. The selection policy now retains an
  observed destination across the app's own UI and clears it on another app;
  all six tests pass. Native code still revalidates the retained AX window/title.
- The built app was restarted successfully with Accessibility, Input Monitoring,
  and keyboard listener diagnostics all reporting granted/available. Its floating
  bar context menu was opened and remote mode was switched on through the UI.
- A native fixture review attempt remained blocked: the app's foreground-app
  observation did not match the RustDesk window addressed by the UI automation
  tool. Diagnostics reported `own_app=false`, `rustdesk=false`; no copy occurred.
  This is an unverified positive path, not a passing remote review test. A manually
  focused disposable remote editor is needed to distinguish automation behavior
  from a remaining native issue. No remote fields were populated by the app.

## Remaining work

Adapter-error regression: red commit `172c0cc` added two tests (one failed,
one already passed). The controller now accepts errors only from its active
attempt and immediately ends it, including an uncertain paste dispatch. Late
success/error callbacks cannot change the outcome or invalidate a new explicit
retry. All 14 controller tests pass. This API awaits a real transport adapter;
it is not native delivery evidence.

- Establish version-specific active-session and clipboard-receipt observability;
  test clipboard isolation with multiple sessions.
- Integrate the implemented clipboard lease and resolve native race/deadline
  limitations; build session, transport, and paste dispatcher contracts.
- Validate the wired manual fallback in a real RustDesk session and implement the
  automatic path only when destination and transport observations are reliable.
- Resolve local hotkey forwarding for automatic mode. The manual fallback uses
  menu Start/Stop and does not use Control to trigger recording.
- Complete the macOS and Linux native matrix in #13 using disposable inputs.
- Complete the native fallback checks; receipt remains unobservable and no
  automatic insertion is claimed.
- Complete [draft PR #14](https://github.com/nolanmak/Murmur/pull/14) after
  the outstanding checks in the [acceptance audit](remote-acceptance.md).

No end-to-end estimate is revised yet: the transport feasibility gate is open.

## Linux-host follow-up: macOS bundle identity

Source inspection found an actionable mismatch: the observer accepted only
`com.carriez.rustdesk`, but the official RustDesk 1.4.6
[macOS build configuration](https://github.com/rustdesk/rustdesk/blob/1.4.6/flutter/macos/Runner/Configs/AppInfo.xcconfig)
declares `com.carriez.flutterHbb`. Selection and redacted diagnostics now use the
same exact allowlist. No prefix matching or title-only app identification was
introduced. A regression test failed for the Flutter identity before the fix and
passes afterward; unrelated applications and suffix lookalikes remain rejected.
This is a plausible cause of the previously blocked selection, not native proof
that the positive Mac-to-Linux flow now passes. This follow-up was developed on
Ubuntu 24.04 / GNOME 46 / X11 with RustDesk host 1.4.6. macOS CI builds an app
artifact for the remaining client-side validation. Issue #13 remains open.
