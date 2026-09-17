//! Print what the Deepgram stream actually does, event by event.
//!
//! `session::run` consumes only `StreamEvent::Final` and drops everything
//! else, so a stream that never connects is indistinguishable from a meeting
//! where nobody spoke. This prints the whole event sequence instead.
//!
//! Usage: DEEPGRAM_API_KEY=... cargo run -p fotw-stt --example dg_probe

use std::time::Duration;

use fotw_stt::deepgram::DeepgramConfig;
use fotw_stt::{DeepgramStream, DeepgramStreamConfig, Source, StreamEvent};

#[tokio::main]
async fn main() {
    let Ok(key) = std::env::var("DEEPGRAM_API_KEY") else {
        eprintln!("set DEEPGRAM_API_KEY");
        return;
    };
    println!("key length: {} chars", key.len());

    let config = DeepgramStreamConfig::new(
        key,
        DeepgramConfig::new("dg-probe".to_owned(), Source::System),
    );

    let (stream, mut events) = DeepgramStream::open(config);
    println!("stream opened, feeding 3s of a 440 Hz tone...");

    // A tone rather than silence: some providers close an idle stream, and a
    // stream that closed because we sent nothing would look like a failure.
    tokio::spawn(async move {
        for chunk in 0..30 {
            let mut pcm = Vec::with_capacity(1600);
            for i in 0..1600 {
                let t = (chunk * 1600 + i) as f32 / 16_000.0;
                pcm.push(((t * 440.0 * std::f32::consts::TAU).sin() * 8000.0) as i16);
            }
            stream.write(&pcm);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let _ = stream.flush().await;
        let _ = stream.close().await;
    });

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            println!("--- 20s elapsed, giving up ---");
            break;
        }
        match tokio::time::timeout(left, events.recv()).await {
            Err(_) => {
                println!("--- timed out waiting for an event ---");
                break;
            }
            Ok(None) => {
                println!("--- event channel closed ---");
                break;
            }
            Ok(Some(ev)) => match ev {
                StreamEvent::State(s) => println!("STATE   {s:?}"),
                StreamEvent::Error(e) => println!("ERROR   {e:?}"),
                StreamEvent::Final(seg) => println!("FINAL   {:?}", seg.text),
                other => println!("OTHER   {other:?}"),
            },
        }
    }
}
