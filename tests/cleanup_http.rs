use murmur::{
    cleanup::{Disposition, Privacy},
    cleanup_config::{NetworkPolicy, Settings},
    vocabulary::{Profile, Snippet},
};
use std::{
    sync::{Arc, atomic::AtomicU8},
    time::Duration,
};
fn settings(endpoint: String) -> Settings {
    Settings {
        enabled: true,
        endpoint,
        model: "fixture-model".into(),
        ..Default::default()
    }
}
#[test]
fn endpoint_and_schema_validation_refuse_credential_leaks_and_unbounded_waits() {
    for url in [
        "http://provider.example/v1/chat/completions",
        "https://user:secret@provider.example/v1/chat/completions",
        "https://provider.example/v1/chat/completions?key=secret",
        "https://provider.example/v1/chat/completions#fragment",
        "https://provider.example/",
    ] {
        assert!(settings(url.into()).validate().is_err(), "{url}");
    }
    let mut config = settings("https://provider.example/v1/chat/completions".into());
    config.deadline_ms = 60000;
    assert!(config.validate().is_err());
    config.deadline_ms = 1500;
    config.credential_env = "TOKEN=$(unsafe)".into();
    assert!(config.validate().is_err());
}
#[tokio::test]
async fn no_opt_in_no_network_policy_and_snippet_expansion_never_resolve_keys() {
    let profile = Profile {
        snippets: vec![Snippet {
            name: "reply".into(),
            text: "Private expanded snippet body".into(),
        }],
        ..Default::default()
    };
    for (raw, cfg, privacy, want) in [
        ("plain", Settings::default(), Privacy::Configured, "plain"),
        (
            "plain",
            settings("https://provider.example/v1/chat/completions".into()),
            Privacy::OfflineAll,
            "plain",
        ),
        (
            "insert snippet reply",
            settings("https://provider.example/v1/chat/completions".into()),
            Privacy::Configured,
            "Private expanded snippet body",
        ),
    ] {
        let result = murmur::cleanup_http::Service::default()
            .run(
                raw,
                &profile,
                Ok(&cfg),
                privacy,
                Arc::new(AtomicU8::new(1)),
                |_| panic!("credential resolver must not be reached"),
            )
            .await
            .unwrap();
        assert_eq!(result.text, want);
    }
}
#[test]
fn persisted_cleanup_defaults_and_network_policy_are_independent() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("cleanup.json");
    let privacy = root.path().join("network-policy.json");
    assert!(!Settings::load(&file).unwrap().enabled);
    assert!(!NetworkPolicy::load(&privacy).unwrap().offline_all);
    NetworkPolicy {
        version: 1,
        offline_all: true,
    }
    .save(&privacy)
    .unwrap();
    let config = settings("https://provider.example/v1/chat/completions".into());
    config.save(&file).unwrap();
    assert_eq!(Settings::load(&file).unwrap(), config);
    assert!(NetworkPolicy::load(&privacy).unwrap().offline_all);
    std::fs::write(&file, b"broken").unwrap();
    assert!(Settings::load(&file).is_err());
    assert!(NetworkPolicy::load(&privacy).unwrap().offline_all);
}
#[tokio::test]
async fn configured_adapter_sends_only_the_utterance_and_accepts_a_complete_edit_plan() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (sent, received) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut data = Vec::new();
        let mut block = [0u8; 4096];
        loop {
            let n = stream.read(&mut block).await.unwrap();
            if n == 0 {
                return;
            }
            data.extend_from_slice(&block[..n]);
            if let Some(split) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&data[..split]);
                let length: usize = headers
                    .lines()
                    .find_map(|l| {
                        l.to_lowercase()
                            .strip_prefix("content-length: ")
                            .map(str::to_owned)
                    })
                    .unwrap()
                    .parse()
                    .unwrap();
                if data.len() >= split + 4 + length {
                    let body: serde_json::Value =
                        serde_json::from_slice(&data[split + 4..]).unwrap();
                    sent.send((headers.to_string(), body)).unwrap();
                    let body=serde_json::json!({"choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"{\"version\":1,\"remove\":[0],\"breaks\":[]}","refusal":null}}]}).to_string();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    stream.write_all(response.as_bytes()).await.unwrap();
                    return;
                }
            }
        }
    });
    let config = settings(format!("http://{addr}/v1/chat/completions"));
    let profile = Profile::default();
    let result = murmur::cleanup_http::Service::default()
        .run(
            "um Hello Zoë",
            &profile,
            Ok(&config),
            Privacy::Configured,
            Arc::new(AtomicU8::new(1)),
            |name| {
                assert_eq!(name, "MURMUR_CLEANUP_API_KEY");
                Ok(fotw_secrets::SecretString::new("fixture-token"))
            },
        )
        .await
        .unwrap();
    let captured = tokio::time::timeout(Duration::from_millis(200), received).await;
    server.abort();
    let (headers, body) = captured
        .expect("configured provider received no request")
        .unwrap();
    assert!(headers.contains("Bearer fixture-token"));
    assert_eq!(body["model"], "fixture-model");
    assert_eq!(body["store"], false);
    assert_eq!(body["messages"].as_array().unwrap().len(), 2);
    let user: serde_json::Value =
        serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(user["text"], "um Hello Zoë");
    assert_eq!(user["tokens"], serde_json::json!(["um", "Hello", "Zoë"]));
    assert_eq!(result.text, "Hello Zoë");
    assert_eq!(result.disposition, Disposition::Cleaned);
}

#[test]
fn offline_all_uses_local_speech_even_when_cloud_or_fallback_was_configured() {
    for provider in [
        murmur::speech::Provider::Deepgram,
        murmur::speech::Provider::DeepgramThenLocal,
    ] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("speech.json");
        murmur::speech::Settings {
            provider,
            ..Default::default()
        }
        .save(&path)
        .unwrap();
        let selected = murmur::speech::Selection::load(Ok(path));
        assert!(matches!(
            selected
                .capture_provider_with_privacy(true, || panic!(
                    "Offline-all resolved a cloud credential"
                ))
                .unwrap(),
            murmur::capture::CaptureProvider::Local(_)
        ));
    }
}

#[test]
fn configuration_changes_need_a_fresh_destination_review_and_save_failure_preserves_choice() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("cleanup.json");
    let policy = root.path().join("network-policy.json");
    let args = vec![
        "configure".into(),
        "https://provider.example/v1/chat/completions".into(),
        "fixture-model".into(),
        "FIXTURE_TOKEN".into(),
    ];
    murmur::cleanup_config::command(&args, &file, &policy).unwrap();
    let reviewed = Settings::load(&file).unwrap();
    assert!(!reviewed.enabled);
    let mut changed = reviewed.clone();
    changed.endpoint = "https://different.example/v1/chat/completions".into();
    changed.save(&file).unwrap();
    assert!(Settings::set_enabled(&file, &reviewed, true).is_err());
    assert_eq!(Settings::load(&file).unwrap(), changed);
    murmur::cleanup_config::command(&["offline-all".into()], &file, &policy).unwrap();
    assert!(murmur::cleanup_config::command(&["on".into()], &file, &policy).is_err());
    assert!(!Settings::load(&file).unwrap().enabled);
    murmur::cleanup_config::command(&["configured".into()], &file, &policy).unwrap();
    murmur::cleanup_config::command(&["on".into()], &file, &policy).unwrap();
    assert!(Settings::load(&file).unwrap().enabled);
    let mut name = file.file_name().unwrap().to_os_string();
    name.push(".lock");
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .open(root.path().join(name))
        .unwrap();
    lock.try_lock().unwrap();
    assert!(Settings::set_enabled(&file, &Settings::load(&file).unwrap(), false).is_err());
    assert!(Settings::load(&file).unwrap().enabled);
    lock.unlock().unwrap();
}
#[test]
fn cleanup_credential_resolution_is_named_read_only_and_never_needs_keychain() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("fixture.env");
    std::fs::write(
        &file,
        "OTHER_SECRET=unrelated\nFIXTURE_TOKEN='literal-$(not-executed)'\n",
    )
    .unwrap();
    let mut vars = std::collections::HashMap::new();
    let key = murmur::config::resolve_named(&vars, Some(&file), "FIXTURE_TOKEN").unwrap();
    assert_eq!(key.expose(), "literal-$(not-executed)");
    assert!(!format!("{key:?}").contains("not-executed"));
    vars.insert("FIXTURE_TOKEN".into(), "process-fixture".into());
    assert_eq!(
        murmur::config::resolve_named(&vars, Some(&file), "FIXTURE_TOKEN")
            .unwrap()
            .expose(),
        "process-fixture"
    );
    assert!(murmur::config::resolve_named(&vars, Some(&file), "ABSENT_TOKEN").is_err());
}
#[tokio::test]
async fn invalid_cleanup_settings_do_not_lose_raw_text_or_resolve_a_key() {
    let result = murmur::cleanup_http::Service::default()
        .run(
            "Raw text",
            &Profile::default(),
            Err("malformed fixture settings"),
            Privacy::Configured,
            Arc::new(AtomicU8::new(1)),
            |_| panic!("no keys"),
        )
        .await
        .unwrap();
    assert_eq!(result.text, "Raw text");
    assert_eq!(result.disposition, Disposition::Fallback);
}
