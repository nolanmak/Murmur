# Linux implementation scope

Planning only: no Linux support is implemented by this change. Existing cross-platform CI is not native desktop acceptance.

## Target and delivery order

First identify the Linux machine's distribution/version, architecture, desktop/session and available microphone. A headless server cannot provide a global desktop shortcut or focused graphical input. Remote audio forwarding and RustDesk integration are separate work.

Start with capability contracts. Then build audio/manual controls and X11 shortcut/insertion in separate worktrees. Run the Wayland feasibility checkpoint early, before selecting its implementation; do not assume modifier-only Control or universal automatic paste. Package the verified target and finish native release validation. No additional STT provider is needed: reuse Deepgram Nova-3 with user-supplied credentials.

## Issues

- [Linux desktop dictation: delivery plan and release gates](https://github.com/nolanmak/Murmur/issues/15) — depends on target discovery / existing shared core.
- [Linux platform contracts, capability detection and support matrix](https://github.com/nolanmak/Murmur/issues/16) — depends on target discovery / existing shared core.
- [Linux microphone capture and Deepgram lifecycle](https://github.com/nolanmak/Murmur/issues/17) — depends on #16.
- [Linux X11 hold-Control shortcut and cancellation](https://github.com/nolanmak/Murmur/issues/18) — depends on #16.
- [Linux X11 focused text insertion and clipboard recovery](https://github.com/nolanmak/Murmur/issues/19) — depends on #16.
- [Wayland capability spike and supported shortcut/insertion adapter](https://github.com/nolanmak/Murmur/issues/20) — depends on #16.
- [Linux recording indicator, manual controls and permission diagnostics](https://github.com/nolanmak/Murmur/issues/21) — depends on #16, #17.
- [Linux configuration, installation and packaging](https://github.com/nolanmak/Murmur/issues/22) — depends on #16, #17, #21.
- [Linux native acceptance suite and release CI](https://github.com/nolanmak/Murmur/issues/23) — depends on #17, #18, #19, #20, #21, #22.

## Worktree and TDD policy

For each implementation issue, fetch origin and create an issue branch in a new worktree from origin/main. Write the behavioral failing test first, preserve red output, implement minimally, and preserve green output. Refactor with tests passing. Push a linked PR, then remove the temporary worktree only after its work is committed and preserved. Do not delete another session's worktree.

Each issue lists measurable acceptance criteria, its own red/green scenarios, dependencies and shared checks. Mocked portals and providers cover failures in CI; native evidence is mandatory for supported desktop behavior. Unrun native checks stay open. Use only synthetic text and sanitized environment descriptions in public evidence.

## Release boundary

Ship only the distro/session/architecture combinations actually verified. Track X11, native Wayland and XWayland separately. Supported fallback modes must be visible; copying or dispatching a paste event is not proof that text reached an input. No claim of Linux readiness until the release gates pass.
