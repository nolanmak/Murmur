# RustDesk remote dictation — implementation evidence

Issue: [#13](https://github.com/nolanmak/Text-to-speech/issues/13).

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

The source revision above is a research reference, not a claim that it exactly
matches the installed release. Release-specific behavior still needs testing.

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

The controller is not connected to the app UI or RustDesk transport yet.
The existing local insertion path is unchanged.

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

## Remaining work

- Establish version-specific active-session and clipboard-receipt observability;
  test clipboard isolation with multiple sessions.
- Integrate the implemented clipboard lease and resolve native race/deadline
  limitations; build session, transport, and paste dispatcher contracts.
- Connect transcript review, opt-in remote mode, destination selection, paste
  profiles, and error states to the native app.
- Validate local hotkey forwarding behavior inside RustDesk.
- Complete the macOS and Linux native matrix in #13 using disposable inputs.
- If receipt cannot be observed, expose the explicitly scoped copy/manual-paste
  fallback and explain its limitations; do not report automatic insertion.
- Produce setup/troubleshooting documentation and a PR with verified limitations.

No end-to-end estimate is revised yet: the transport feasibility gate is open.
