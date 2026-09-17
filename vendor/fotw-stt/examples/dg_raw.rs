//! Print Deepgram's raw frames, unparsed.
//!
//! The typed reader rejects something with "expected a sequence", and a
//! deserialiser error names an offset rather than a field. This connects with
//! the exact query the app builds and prints what actually arrives.
//!
//! Usage: DEEPGRAM_API_KEY=... cargo run -p fotw-stt --example dg_raw

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;

use fotw_stt::deepgram_wire::DeepgramStreamParams;

#[tokio::main]
async fn main() {
    let Ok(key) = std::env::var("DEEPGRAM_API_KEY") else {
        eprintln!("set DEEPGRAM_API_KEY");
        return;
    };

    let query = DeepgramStreamParams::spec().to_query();
    let url = format!("wss://api.deepgram.com/v1/listen?{query}");
    println!("QUERY: {query}\n");

    let mut request = url.into_client_request().expect("request");
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Token {key}")).expect("header"),
    );

    let (socket, response) = match connect_async(request).await {
        Ok(pair) => pair,
        Err(e) => {
            println!("CONNECT FAILED: {e}");
            return;
        }
    };
    println!("CONNECTED: HTTP {}\n", response.status());

    let (mut tx, mut rx) = socket.split();

    // A tone, not silence: Deepgram emits Results frames either way, and those
    // are what the typed reader is choking on.
    tokio::spawn(async move {
        for chunk in 0..40 {
            let mut pcm = Vec::with_capacity(3200);
            for i in 0..1600i32 {
                let t = (chunk * 1600 + i) as f32 / 16_000.0;
                let v = ((t * 440.0 * std::f32::consts::TAU).sin() * 8000.0) as i16;
                pcm.extend_from_slice(&v.to_le_bytes());
            }
            if tx.send(Message::Binary(pcm.into())).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let _ = tx
            .send(Message::Text("{\"type\":\"CloseStream\"}".into()))
            .await;
    });

    let mut seen = 0;
    while let Ok(Some(msg)) = tokio::time::timeout(Duration::from_secs(12), rx.next()).await {
        match msg {
            Ok(Message::Text(t)) => {
                seen += 1;
                println!("FRAME {seen}: {t}");
                // Show the byte the deserialiser complains about.
                if t.len() > 40 {
                    println!("        col 30..45 = {:?}", &t[30..45.min(t.len())]);
                }
                if seen >= 4 {
                    break;
                }
            }
            Ok(Message::Close(c)) => {
                println!("CLOSE: {c:?}");
                break;
            }
            Ok(_) => {}
            Err(e) => {
                println!("READ ERROR: {e}");
                break;
            }
        }
    }
    println!("--- done, {seen} text frames ---");
}
