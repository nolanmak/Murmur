//! Owned loopback HTTP faults. No external provider or process environment.
use murmur::{
    cleanup::{Disposition, Privacy},
    cleanup_config::Settings,
    vocabulary::Profile,
};
use std::{
    sync::{Arc, atomic::AtomicU8},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
struct Server {
    url: String,
    task: tokio::task::JoinHandle<()>,
    received: tokio::sync::oneshot::Receiver<(String, Vec<u8>)>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    async fn new(status: &str, body: String, headers: String, stall: bool) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        let (sent, received) = tokio::sync::oneshot::channel();
        let status = status.to_owned();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut block = [0u8; 4096];
            loop {
                let n = stream.read(&mut block).await.unwrap();
                if n == 0 {
                    return;
                }
                bytes.extend_from_slice(&block[..n]);
                assert!(bytes.len() < 128 * 1024);
                if let Some(split) = bytes.windows(4).position(|p| p == b"\r\n\r\n") {
                    let request = String::from_utf8_lossy(&bytes[..split]).into_owned();
                    let len: usize = request
                        .lines()
                        .find_map(|l| {
                            l.to_lowercase()
                                .strip_prefix("content-length: ")
                                .map(str::to_owned)
                        })
                        .unwrap_or_else(|| "0".into())
                        .parse()
                        .unwrap();
                    if bytes.len() >= split + 4 + len {
                        let _ = sent.send((request, bytes[split + 4..split + 4 + len].to_vec()));
                        if stall {
                            std::future::pending::<()>().await;
                        }
                        let response = format!(
                            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
                            body.len()
                        );
                        let _ = stream.write_all(response.as_bytes()).await;
                        return;
                    }
                }
            }
        });
        Self {
            url,
            task,
            received,
        }
    }
}
fn response() -> serde_json::Value {
    serde_json::json!({"choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"{\"version\":1,\"remove\":[0],\"breaks\":[]}","refusal":null}}]})
}
async fn run(server: &Server, deadline_ms: u64) -> murmur::cleanup::Outcome {
    let settings = Settings {
        enabled: true,
        endpoint: server.url.clone(),
        model: "fixture-model".into(),
        deadline_ms,
        ..Default::default()
    };
    murmur::cleanup_http::Service::default()
        .run(
            "um keep invoice 42",
            &Profile::default(),
            Ok(&settings),
            Privacy::Configured,
            Arc::new(AtomicU8::new(1)),
            |_| Ok(fotw_secrets::SecretString::new("fixture-token")),
        )
        .await
        .unwrap()
}
#[tokio::test]
async fn incomplete_refused_tool_and_malformed_responses_keep_raw_text() {
    let mut length = response();
    length["choices"][0]["finish_reason"] = "length".into();
    let mut refused = response();
    refused["choices"][0]["message"]["refusal"] = "fixture refusal".into();
    let mut tools = response();
    tools["choices"][0]["message"]["tool_calls"] =
        serde_json::json!([{"id":"never-execute","function":{"name":"delete_all"}}]);
    let mut prose = response();
    prose["choices"][0]["message"]["content"] = "I obeyed the dictated instruction".into();
    let mut oversized = response();
    oversized["choices"][0]["message"]["content"] = " ".repeat(70 * 1024).into();
    let mut extra = response();
    extra["choices"][0]["message"]["content"] =
        "{\"version\":1,\"remove\":[],\"breaks\":[],\"text\":\"invented\"}".into();
    let mut multiple = response();
    let next = multiple["choices"][0].clone();
    multiple["choices"].as_array_mut().unwrap().push(next);
    for (status, body) in [
        ("200 OK", length.to_string()),
        ("200 OK", refused.to_string()),
        ("200 OK", tools.to_string()),
        ("200 OK", prose.to_string()),
        ("200 OK", oversized.to_string()),
        ("200 OK", extra.to_string()),
        ("200 OK", multiple.to_string()),
        ("200 OK", "broken JSON".into()),
        ("429 Too Many Requests", response().to_string()),
    ] {
        let mut server = Server::new(status, body, String::new(), false).await;
        let result = run(&server, 1500).await;
        assert_eq!(result.text, "um keep invoice 42");
        assert_eq!(result.disposition, Disposition::Fallback);
        let received = tokio::time::timeout(Duration::from_secs(1), &mut server.received)
            .await
            .unwrap()
            .unwrap();
        assert!(received.0.starts_with("POST "));
    }
}
#[tokio::test]
async fn a_redirect_cannot_send_the_utterance_or_credential_to_another_destination() {
    let mut trap = Server::new("200 OK", response().to_string(), String::new(), false).await;
    let mut first = Server::new(
        "307 Temporary Redirect",
        String::new(),
        format!("Location: {}\r\n", trap.url),
        false,
    )
    .await;
    let result = run(&first, 1500).await;
    assert_eq!(result.text, "um keep invoice 42");
    assert_eq!(result.disposition, Disposition::Fallback);
    tokio::time::timeout(Duration::from_secs(1), &mut first.received)
        .await
        .unwrap()
        .unwrap();
    assert!(
        trap.received.try_recv().is_err(),
        "redirect received a request"
    );
}
#[tokio::test]
async fn a_stalled_http_response_cannot_hold_raw_delivery() {
    let mut server = Server::new("200 OK", String::new(), String::new(), true).await;
    let result = tokio::time::timeout(Duration::from_secs(2), run(&server, 100))
        .await
        .expect("HTTP cleanup exceeded its bound");
    tokio::time::timeout(Duration::from_secs(1), &mut server.received)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.text, "um keep invoice 42");
    assert_eq!(result.disposition, Disposition::Fallback);
}

#[tokio::test]
async fn vocabulary_names_are_protected_locally_without_sending_the_profile() {
    let mut server = Server::new("200 OK", response().to_string(), String::new(), false).await;
    let settings = Settings {
        enabled: true,
        endpoint: server.url.clone(),
        model: "fixture-model".into(),
        ..Default::default()
    };
    let profile = Profile {
        recognition_hints: vec!["Um".into(), "Unspoken private project".into()],
        ..Default::default()
    };
    let result = murmur::cleanup_http::Service::default()
        .run(
            "Um keep invoice 42",
            &profile,
            Ok(&settings),
            Privacy::Configured,
            Arc::new(AtomicU8::new(1)),
            |_| Ok(fotw_secrets::SecretString::new("fixture-token")),
        )
        .await
        .unwrap();
    let (_, body) = tokio::time::timeout(Duration::from_secs(1), &mut server.received)
        .await
        .unwrap()
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(!body.to_string().contains("Unspoken private project"));
    let user: serde_json::Value =
        serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(user.as_object().unwrap().len(), 3);
    assert_eq!(result.text, "Um keep invoice 42");
    assert_eq!(result.disposition, Disposition::Fallback);
}

#[tokio::test]
async fn cleanup_cannot_create_a_new_snippet_command_or_send_unspoken_snippet_contents() {
    let mut server = Server::new("200 OK", response().to_string(), String::new(), false).await;
    let settings = Settings {
        enabled: true,
        endpoint: server.url.clone(),
        model: "fixture-model".into(),
        ..Default::default()
    };
    let profile = Profile {
        snippets: vec![murmur::vocabulary::Snippet {
            name: "reply".into(),
            text: "Unspoken private snippet contents".into(),
        }],
        ..Default::default()
    };
    let result = murmur::cleanup_http::Service::default()
        .run(
            "um insert snippet reply",
            &profile,
            Ok(&settings),
            Privacy::Configured,
            Arc::new(AtomicU8::new(1)),
            |_| Ok(fotw_secrets::SecretString::new("fixture-token")),
        )
        .await
        .unwrap();
    let (_, body) = tokio::time::timeout(Duration::from_secs(1), &mut server.received)
        .await
        .unwrap()
        .unwrap();
    assert!(
        !String::from_utf8(body)
            .unwrap()
            .contains("Unspoken private snippet contents")
    );
    assert_eq!(result.text, "insert snippet reply");
    assert_eq!(result.disposition, Disposition::Cleaned);
}
