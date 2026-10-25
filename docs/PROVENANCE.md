# Shared FlyOnTheWall components

Source: https://github.com/nolanmak/FlyOnTheWall
Pinned revision: `33375fe7baf0ef6c1d15dd66f8ee8cc949bb8e03`
License: Apache-2.0; root LICENSE and NOTICE retained.

Murmur resolves four crates from that immutable Git revision, with matching
normal/dev-dependency pins and Cargo.lock. There is no sibling-checkout path
requirement or moving-branch dependency:

- `fotw-audio`: capture types, microphone platform implementation and fixture seam.
- `fotw-stt`: transcript types, Deepgram wire parameters/normalization and the versioned vocabulary schema.
- `fotw-secrets`: credential namespace, redacted values and platform stores.
- `fotw-dsp`: resampling and PCM conversion, re-exported by `src/resample.rs`.

The former vendor snapshot was `5cfe8562877823fa5403a3e9cd340996d575ce33`.
Those frozen source copies are removed. The extracted DSP implementation is
byte-for-byte identical to Murmur's previous `src/resample.rs`; application
capture/deadline/cleanup and insertion policy remain in Murmur.

The native shell patterns (nonactivating AppKit panel, main-thread timer and
tray lifecycle) remain adapted from fotw-shell. The dictation controller,
focus checks and CloseStream-based transport remain application-owned.
Murmur waits for Deepgram's final Metadata after CloseStream, retaining its
existing empty-recording and endpointing behavior.

## Runtime and credential boundaries

Murmur does not enable the shared crate's `local-whisper` feature: its existing
external helper owns local inference. Importing DSP does not import meeting
storage or codecs. Shared source code does not share loaded model memory.

The producer's explicit credential stores preserve the same service/account
addresses. macOS uses its existing native Keychain store; Linux Secret Service
uses the DBus backend whose binary read/update/delete compatibility is checked
against the previous backend on an isolated bus in the producer repository.
Murmur's config precedence and local-only credential bypass are unchanged.

## Checks and upgrades

`tests/shared_credentials.rs` and `tests/shared_speech.rs` invoke producer-owned
contracts through dev-only `conformance` features, using synthetic data and no
real credentials or microphone. Murmur's capture, insertion, configuration and
transport regressions remain in this repository. The producer separately tests
its crates and an immutable migration-consumer revision on macOS and Linux.

Upgrade all eight manifest revision fields and Cargo.lock together after
reviewing the producer change. Run both repositories' required checks and the
consumer conformance suite. Roll back manifest and lockfile changes together.
A dependency version change does not imply automatic fallback, device recovery
or local inference policies have been changed.

## Native format upgrade (2026-10-24)

This revision includes merged FlyOnTheWall #137/#138/#139: model-language
validation, bounded supervisor recovery, and actual microphone format reporting.
Murmur #66 already preserves stereo frames across ring drains, so it can consume
the actual multichannel layout without discarding split frames. Unsupported
layouts fail explicitly; callback layout changes are surfaced as capture faults.
Murmur still cancels an invalid capture instead of silently inserting partial text.

The shared supervisor policy does not make Murmur use that supervisor, and the
shared Whisper adapter remains disabled: dictation still uses its managed helper.
This dependency upgrade does not establish native device/sleep acceptance or
implement the remaining Murmur device-recovery work in #61/#58.

## Device notification consumer

The producer revision now includes merged FlyOnTheWall PRs
[#140](https://github.com/nolanmak/FlyOnTheWall/pull/140),
[#141](https://github.com/nolanmak/FlyOnTheWall/pull/141) and
[#142](https://github.com/nolanmak/FlyOnTheWall/pull/142): capture-health APIs,
callback retirement, a continuous macOS capture clock and rollback of partially
registered device listeners. Murmur uses the corrected watcher registration for
its per-attempt default-input notifications. It still owns its dictation policy
and does not automatically adopt the meeting supervisor. Native input/format/
sleep notification coverage and bounded native preparation/teardown remain #61
work; changing the shared clock alone does not complete those policies.


## Explicit native registration cleanup upgrade (2026-10-24)

The pinned producer includes FlyOnTheWall #146: callback unregistration, ordered
native cleanup, positive Stop observation, and typed release evidence. Murmur
uses this evidence to keep a known-stopped microphone distinct from unresolved
capture, hold ownership through cleanup, and allow a new attempt only after
resource release. Neither an error nor a cleanup timeout permits partial insertion.
The native/device acceptance gates in `docs/microphone-startup.md` remain open.


## Partial native setup ownership upgrade (2026-10-24)

The pinned producer revision also includes FlyOnTheWall #147: process-tap and
aggregate acquisition enter the explicit owner before property/callback setup,
and failed release propagates typed cleanup evidence. The public shared seam is
unchanged; consumer conformance and bounded microphone ownership remain covered
by this repository's tests.


## Native input format guard

This immutable producer includes FlyOnTheWall [#162](https://github.com/nolanmak/FlyOnTheWall/pull/162): per-capture device/input-stream format listeners, stale-rate IO rejection, non-authoritative invalidated formats, owned listener teardown and positive Released evidence after a format fault discovered at completed Stop. The shared macOS `formats()` conformance now drives the actual native callback/latch/IO/finalization path with synthetic PCM and no capture device.

Murmur names `FormatChangedAfterStop` as a format fault, returns no transcript for automatic insertion and releases the startup lease only using the SDK’s positive cleanup evidence. A fixture checks that the same controller can complete fresh dictation after that fault. Retained/unconfirmed cleanup remains blocked. Native USB/Bluetooth/rate-change and next-field insertion are still unverified; the producer’s read-only probe reported no default input on this Mac. Background readiness episodes and sustained-health policy remain unfinished.

## Read-only input readiness snapshot

The pinned producer includes FlyOnTheWall #163: real default-input UID and
validated format observations without creating capture resources. Murmur owns
the background worker lease, retry budget, health window and cancellation policy.
A snapshot is advisory metadata, not proof of permissions or live audio.

## Shared vocabulary schema

The profile, snippet type, JSON parsing, validation, language syntax and exact
cue matching now come from `fotw_stt::vocabulary`. The shared producer extracted
these semantics from Murmur revision `9c6723f07e6996dd0aeca87919211cc5519cc7cc`
and retains Murmur copyright in its NOTICE. Version-1 JSON, language defaults
and application limits are unchanged; no profile migration is needed.

Murmur's `vocabulary::ProfileFile` extension retains application-specific paths,
bounded reads, nonblocking writer locking and atomic replacement. Native editor
stale-draft/field-scoped merge policy remains here. Local and cloud adapters use
the shared hint capability assessment; snippets never enter provider prompts.
`tests/shared_speech.rs` also runs the producer-owned vocabulary fixture matrix.
The application-level writer, transport and fallback tests remain here. Neither
code sharing nor these fixtures prove a term-recall improvement; #9/#118 keep
the human/model quality and physical interaction gates open.
