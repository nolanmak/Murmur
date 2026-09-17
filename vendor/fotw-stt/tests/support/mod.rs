//! Shared test scaffolding for the Deepgram streaming transport.
//!
//! Nothing here touches the network beyond `127.0.0.1`, and nothing needs a key.
//! That is a hard requirement, not a preference: CI runs with no secrets, so a
//! transport that can only be exercised against Deepgram is a transport that is
//! never exercised at all.

// Each integration test binary compiles this module separately and uses a
// different slice of it, so unused-item warnings here are structural rather
// than informative.
#![allow(dead_code)]

pub mod mock_deepgram;
pub mod pcm;
pub mod script;
pub mod wer;

use std::time::Duration;

/// Poll `predicate` until it holds or `timeout` elapses.
///
/// The alternative — sleeping for "long enough" and then asserting — is the
/// thing that makes async test suites flaky on loaded CI. This waits on the
/// condition and only uses the clock as a failure bound.
pub async fn wait_until<F>(timeout: Duration, mut predicate: F) -> bool
where
    F: FnMut() -> bool,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if predicate() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// A tiny seeded LCG, so "random" kill points are reproducible.
///
/// A chaos test whose failures cannot be replayed is a chaos test that gets
/// deleted the first time it goes red on someone else's machine.
#[derive(Debug, Clone)]
pub struct Lcg(u64);

impl Lcg {
    /// A generator with an explicit seed.
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// The next value in `0..bound`.
    pub fn next_below(&mut self, bound: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        if bound == 0 {
            0
        } else {
            (self.0 >> 33) % bound
        }
    }
}
