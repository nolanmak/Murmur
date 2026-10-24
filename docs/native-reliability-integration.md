# Native reliability integration

The offline-suite branch integrates existing work without treating historical
native reports as evidence for the current combined build:

- PR #48, commit 627b41d: permission snapshot, route gate, named warnings,
  sequential onboarding, permission rechecks and Reopen Murmur.
- PR #50, commit 965a1e4: up to 750 ms of focus settling before local capture,
  immediate secure-field rejection, and refusal when no field appears.
- The prewarming helper from PR #50's 1c95a7d is adapted separately. It requests
  frontmost app accessibility while idle and already permitted. The later
  record-without-focus/late-target behavior is not adopted because #49 explicitly
  requires refusing local capture when no text field is found.
- PR #47, commit e479024: stage, sign, verify and replace the bundle; stable local
  identity discovery and an explicit MURMUR_REQUIRE_IDENTITY gate. New injected
  move-failure tests exposed a missing rollback and deletion of a pre-existing
  `.previous` path. The script now owns a unique backup and restores the previous
  bundle if installation fails. If restoration itself fails it retains the
  backup and reports its location rather than falsely claiming restoration.
- PR #53 is adapted with bounded outstanding startup workers as described in
  [microphone startup](microphone-startup.md).

Local validation: full locked workspace tests, Clippy with warnings denied,
formatting and repository boundaries pass. Focus/permission fixtures and all
12 signing-script tests use fakes; no test credential store is a real keychain.
New startup and signing regressions were observed failing before the fixes.

The strict native build refused installation because this machine initially had
no usable signing identity, preserving its existing bundle. The documented
FlyOnTheWall dev-sign setup is being used for a shared local development identity;
macOS authentication for certificate trust is required before signed-app QA.
No signed native acceptance result is claimed yet. Real Chrome/Discord focus,
menu reopening, hotkey/insertion, microphone routes, and sleep/wake remain gates
for #45/#49/#58/#61. No corresponding issue is closed by these local tests.
