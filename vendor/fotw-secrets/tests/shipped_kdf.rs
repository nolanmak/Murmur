//! The KDF parameters we actually ship.
//!
//! Every other recovery test runs on deliberately cheap parameters, because a
//! suite that pays 64 MiB of Argon2 per case is a suite nobody runs. The cost
//! of that choice is that **the shipped defaults were exercised nowhere**: a
//! `KdfParams::default()` that the `argon2` crate rejects outright, or that
//! silently disagrees between wrap and unwrap, would have passed the entire
//! suite and failed on a real user's first run — at the exact moment they are
//! creating a library and have no recovery path yet.
//!
//! So this file is slow on purpose, and it is the only one that is.
//!
//! Measured on an M-series Mac (macOS 26.3), m = 64 MiB, t = 3, p = 1:
//!
//! | build   | wrap    | unwrap  |
//! |---------|---------|---------|
//! | release | 93 ms   | 87 ms   |
//! | debug   | 1216 ms | 1176 ms |
//!
//! Release is what a user pays, and ~90 ms is the right order for a thing that
//! happens once at library creation and once at recovery. There is no timing
//! *assertion* here: a wall-clock threshold on shared CI hardware fails for
//! reasons that have nothing to do with this code, and a flaky guard gets
//! deleted or ignored, which is worse than no guard.

use fotw_secrets::recovery::{KdfParams, MasterKeyBytes, RecoveryKey, WrappedMasterKey};

#[test]
fn the_shipped_parameters_round_trip_a_master_key() {
    let recovery = RecoveryKey::generate().expect("generate a recovery key");
    let master = MasterKeyBytes::from_slice(&[0x42u8; 32]).expect("32 bytes");
    let params = KdfParams::default();

    let started = std::time::Instant::now();
    let blob = WrappedMasterKey::wrap(&master, &recovery, params).expect("wrap");
    let wrap = started.elapsed();

    let started = std::time::Instant::now();
    let out = blob
        .unwrap_master(&recovery, std::path::Path::new("recovery-blob"))
        .expect("unwrap with the correct key");
    let unwrap = started.elapsed();

    // Printed rather than asserted, so `--nocapture` re-measures the table
    // above on whatever machine is in front of you.
    println!(
        "m={} KiB t={}  wrap {:?}  unwrap {:?}",
        params.m_cost_kib, params.t_cost, wrap, unwrap
    );

    assert_eq!(
        out.expose(),
        &[0x42u8; 32],
        "the shipped parameters did not recover the master key"
    );
}

/// The negative, at shipped parameters.
///
/// Worth paying the seconds for: a wrap/unwrap pair that ignored the recovery
/// key entirely would pass the test above and fail this one.
#[test]
fn the_shipped_parameters_reject_the_wrong_recovery_key() {
    let right = RecoveryKey::generate().expect("generate");
    let wrong = RecoveryKey::generate().expect("generate");
    let master = MasterKeyBytes::from_slice(&[0x42u8; 32]).expect("32 bytes");

    let blob = WrappedMasterKey::wrap(&master, &right, KdfParams::default()).expect("wrap");
    let err = blob
        .unwrap_master(&wrong, std::path::Path::new("recovery-blob"))
        .expect_err("the wrong recovery key unwrapped the master key");

    // Not "the database is corrupt" — that message sends a user hunting a
    // data-corruption bug that does not exist. Issue #38 is explicit about it.
    assert!(
        err.is_wrong_key(),
        "a wrong key must be reported as a wrong key, got: {err}"
    );
}
