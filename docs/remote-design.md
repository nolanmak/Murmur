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

The controller is not connected to the app UI or a native transport yet.
The existing local insertion path is unchanged.

## TDD evidence

Command: `cargo test --locked --test remote`.

- Red commit `8395a31`: 12 behavioral tests failed against the compiling controller
  stub, including explicit sharing, session drift, cancellation, timeout bounds,
  and paste profiles.
- Green implementation: the same 12 tests pass without weakening assertions.
- These are controller tests. No native RustDesk acceptance row is marked passed.

## Remaining work

- Establish version-specific active-session and clipboard-receipt observability;
  test clipboard isolation with multiple sessions.
- Implement and test clipboard ownership, lossless snapshots, recovery, and
  injectable adapter contracts.
- Connect transcript review, opt-in remote mode, destination selection, paste
  profiles, and error states to the native app.
- Validate local hotkey forwarding behavior inside RustDesk.
- Complete the macOS and Linux native matrix in #13 using disposable inputs.
- If receipt cannot be observed, expose the explicitly scoped copy/manual-paste
  fallback and explain its limitations; do not report automatic insertion.
- Produce setup/troubleshooting documentation and a PR with verified limitations.

No end-to-end estimate is revised yet: the transport feasibility gate is open.
