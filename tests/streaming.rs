use fotw_stt::DeepgramEndpoint;
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use text_to_speech::streaming::transcribe;
use tokio::{net::TcpListener, sync::mpsc};
use tokio_tungstenite::{
    accept_hdr_async,
    tungstenite::{
        Message,
        handshake::server::{Request, Response},
    },
};
#[allow(
    clippy::result_large_err,
    reason = "tungstenite fixes the handshake callback error type"
)]
async fn run_mock(mode: u8) -> Result<String, String> {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut ws = accept_hdr_async(socket, |req: &Request, res: Response| {
            assert_eq!(req.headers()["authorization"], "Token test-key");
            assert!(req.uri().query().unwrap().contains("mip_opt_out=true"));
            Ok(res)
        })
        .await
        .unwrap();
        let mut binary = false;
        while let Some(Ok(msg)) = ws.next().await {
            if let Message::Binary(bytes) = &msg {
                assert!(!bytes.is_empty());
                binary = true;
            }
            if let Message::Text(text) = msg
                && text.contains("CloseStream")
            {
                assert!(binary);
                ws.send(Message::text(r#"{"type":"Results","is_final":false,"speech_final":false,"channel":{"alternatives":[{"transcript":"wrong interim"}]}}"#)).await.unwrap();
                tokio::time::sleep(Duration::from_millis(30)).await;
                if mode == 1 {
                    ws.send(Message::text(r#"{"type":"Results","is_final":true,"speech_final":true,"from_finalize":false,"channel":{"alternatives":[{"transcript":"Hello world."}]}}"#)).await.unwrap();
                    ws.send(Message::text(r#"{"type":"Metadata","duration":0.01}"#))
                        .await
                        .unwrap();
                } else if mode == 2 {
                    ws.send(Message::text(r#"{"type":"Metadata","duration":0.01}"#))
                        .await
                        .unwrap();
                } else if mode == 3 {
                    tokio::time::sleep(Duration::from_millis(400)).await;
                } else {
                    ws.close(None).await.unwrap();
                }
                break;
            }
        }
    });
    let (tx, rx) = mpsc::channel(4);
    tx.send(vec![0; 160]).await.unwrap();
    drop(tx);
    let result = transcribe(
        "test-key".into(),
        rx,
        DeepgramEndpoint::loopback(port),
        Duration::from_millis(250),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("client never drove the provider")
        .unwrap();
    result
}
#[tokio::test]
async fn release_waits_for_trailing_final_and_never_commits_interim() {
    assert_eq!(run_mock(1).await.unwrap(), "Hello world.");
}
#[tokio::test]
async fn closed_socket_without_completion_metadata_does_not_insert_partial_text() {
    assert!(run_mock(0).await.is_err());
}

#[tokio::test]
async fn empty_or_interim_only_session_completes_without_inventing_text() {
    assert_eq!(run_mock(2).await.unwrap(), "");
}
#[tokio::test]
async fn finalization_deadline_is_bounded() {
    assert!(run_mock(3).await.unwrap_err().contains("timed out"));
}
#[tokio::test]
async fn unencrypted_remote_provider_is_rejected_before_connecting() {
    let (_tx, rx) = mpsc::channel(1);
    let result = transcribe(
        "test-key".into(),
        rx,
        DeepgramEndpoint::insecure("example.com", 80),
        Duration::from_millis(1),
    )
    .await;
    assert!(result.unwrap_err().contains("Unencrypted"));
}
