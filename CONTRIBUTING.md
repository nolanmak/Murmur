# Contributing

Start from a GitHub issue with measurable acceptance criteria.

1. Add a failing behavioral test and record the command/failure (red).
2. Implement the smallest correct change (green).
3. Refactor without weakening assertions; run the relevant regression tests.
4. Run `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D warnings`,
   and `cargo test --locked --workspace` before a PR.
5. Link tests and manual evidence to the issue criteria. Never close an issue whose
   native acceptance checks have not been performed.

Use fixture audio, fake Keychain implementations, injected time and local WebSocket
servers. CI must not access microphones, user files or real provider keys. Avoid
snapshot-only tests or tests that simply repeat implementation details.

Keep native code in src/platform; no unsafe code in the application core. Preserve
upstream attribution when adapting vendor sources. Never commit .env, audio,
transcripts, signing material or build artifacts. Keep this repository private
until the owner explicitly authorizes publishing it.
