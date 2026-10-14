# Login startup QA

Issue: https://github.com/nolanmak/Murmur/issues/54

## Automated behavior and native smoke

```sh
cargo test --locked --test login_item
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --workspace
python3 scripts/check-repo.py
./scripts/build-app.sh
python3 scripts/check-login-startup.py Murmur.app
```

The last command requires a macOS graphical session. It launches the actual app
executable with a temporary HOME, checks the app creates its own valid registration
within 10 seconds and initializes its shell, and terminates only that child. It
never writes a registration itself or runs launchctl. Do not press dictation
shortcuts during this check. HOME isolation is not a separate macOS account and
does not prove OS login behavior. Run the full matrix in a disposable test account.

## Native acceptance matrix (required before closing #54)

Use the final packaged artifact in a permanent location. Record macOS version,
commit, SHA-256 of `Contents/MacOS/murmur`, and timing for every session. Do not
publish personal paths, dictation, credentials, or signing information.

1. In a clean test account with no registration or opt-out marker, launch the app.
   Within 10 seconds, there must be one valid registration targeting that bundle.
2. Disable macOS's Reopen windows option. Perform two full restarts. On each login,
   within 30 seconds of the desktop appearing, verify exactly one Murmur process
   from the tested bundle, one Control menu item, and no recording indicator.
3. Repeat the preceding check for one logout/login cycle.
4. Toggle Open at login Off. Relaunch the app twice; the setting must stay Off.
   Restart and sign in; no Murmur process may appear for 30 seconds.
5. Launch manually and toggle On. Restart/sign in; repeat step 2's checks.
6. Move the app to a path with spaces and XML-sensitive characters, launch it there,
   and check the parsed plist targets only the new location. Repeat restart check.
7. In the test account only, deny writes to LaunchAgents and exercise setup/retry.
   A setup-failed menu item must appear instead of On. Restore access and retry;
   verify the requested setting is applied. Repeat for an invalid registration.
8. With approved permissions, use synthetic text in a disposable field to check
   dictation start/stop/cancel and insertion after login. Do not capture user data.

Do not interpret an exited `/usr/bin/open` launcher as an app crash. Successful
launchers normally exit zero; verify the Murmur process separately. A manually
installed LaunchAgent is recovery only and must be absent for first-launch QA.

## Recorded red/green evidence

On the affected build, `check-login-startup.py` failed with
`healthy app did not create its login registration within 10 seconds` while the
app process remained alive. The initial integration test did not compile on main
because `murmur::login_item` was missing. After importing PR #51, the three new
regressions failed: invalid registration reported enabled, explicit opt-out lost
on failed enable, and leftover registration surviving opt-out. All are covered
by the repaired implementation. Native restart results must be recorded on #54;
they are not inferred from unit tests or this document.
