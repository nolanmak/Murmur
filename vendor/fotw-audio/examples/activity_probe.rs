//! Print what the meeting detector can see, once, and exit.
//!
//! `cargo run -p fotw-audio --example activity_probe`
//!
//! The detector's inputs are a machine state that cannot be reconstructed on
//! CI — "Zoom holds the microphone" needs Zoom, a microphone and a call. This
//! is the manual counterpart to `crates/fotwd/tests/detect.rs`: the tests
//! prove the policy, this shows the facts the policy is fed on a real Mac.
//!
//! Nothing here starts a tap, so it needs no TCC grant and records nothing.

use fotw_audio::activity::ActivityProbe;
use fotw_audio::platform;

fn main() {
    let plat = platform::host();
    match plat.snapshot() {
        Ok(snapshot) => {
            match &snapshot.default_input {
                Some(dev) => println!(
                    "default input : {} [{:?}] running_somewhere={} trustworthy={}",
                    dev.name,
                    dev.transport,
                    dev.running_somewhere,
                    dev.transport.mic_activity_is_trustworthy()
                ),
                None => println!("default input : (none)"),
            }
            println!("audio clients : {}", snapshot.clients.len());
            let mut clients: Vec<_> = snapshot.clients.iter().collect();
            clients.sort_by_key(|c| c.pid);
            for c in clients {
                let flags = match (c.running_input, c.running_output) {
                    (true, true) => "in+out",
                    (true, false) => "in    ",
                    (false, true) => "   out",
                    (false, false) => "      ",
                };
                println!(
                    "  {:>7}  {flags}  {}",
                    c.pid,
                    c.bundle_id.as_deref().unwrap_or("(no bundle id)")
                );
            }
        }
        Err(e) => println!("probe failed: {e}"),
    }
}
