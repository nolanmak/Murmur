//! A blocked credential initializer cannot delay raw output or admit more cleanup workers.
use murmur::{
    cleanup::{Disposition, Privacy},
    cleanup_config::Settings,
    cleanup_http::Service,
    vocabulary::Profile,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicUsize, Ordering},
    },
    time::Duration,
};
#[test]
fn preparation_deadline_keeps_raw_delivery_and_retains_the_same_initializer() {
    let service = Service::default();
    let worker = service.clone();
    let control = Arc::new(AtomicU8::new(1));
    let observed = control.clone();
    let (release, gate) = std::sync::mpsc::channel();
    let (entered, began) = std::sync::mpsc::channel();
    let (send, receive) = std::sync::mpsc::channel();
    let settings = Settings {
        enabled: true,
        endpoint: "https://provider.example/v1/chat/completions".into(),
        model: "fixture-model".into(),
        deadline_ms: 100,
        ..Default::default()
    };
    let first = settings.clone();
    let thread = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = rt.block_on(worker.run(
            "Raw text",
            &Profile::default(),
            Ok(&first),
            Privacy::Configured,
            control,
            move |_| {
                entered.send(()).unwrap();
                let _ = gate.recv();
                Err("fixture unavailable".into())
            },
        ));
        let _ = send.send(result);
    });
    began.recv_timeout(Duration::from_secs(2)).unwrap();
    let result = receive.recv_timeout(Duration::from_millis(500));
    let mut retained = false;
    let calls = Arc::new(AtomicUsize::new(0));
    if result.is_ok() {
        retained = service.in_flight();
        let count = calls.clone();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let second = rt
            .block_on(service.run(
                "Next raw text",
                &Profile::default(),
                Ok(&settings),
                Privacy::Configured,
                Arc::new(AtomicU8::new(1)),
                move |_| {
                    count.fetch_add(1, Ordering::SeqCst);
                    Err("must not resolve another key".into())
                },
            ))
            .unwrap();
        assert_eq!(second.text, "Next raw text");
    }
    release.send(()).unwrap();
    thread.join().unwrap();
    let result = result
        .expect("credential preparation exceeded the cleanup deadline")
        .unwrap();
    assert_eq!(result.text, "Raw text");
    assert_eq!(result.disposition, Disposition::Fallback);
    assert!(retained, "timed-out preparation still owns the one worker");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        observed.load(Ordering::Acquire),
        1,
        "cleanup timeout must not cancel raw dictation delivery"
    );
}

#[tokio::test]
async fn timeout_cancel_and_dropped_future_cannot_send_after_late_preparation() {
    for action in ["timeout", "cancel", "drop"] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let service = Service::default();
        let owned = service.clone();
        let settings = Settings {
            enabled: true,
            endpoint: format!(
                "http://{}/v1/chat/completions",
                listener.local_addr().unwrap()
            ),
            model: "fixture-model".into(),
            deadline_ms: 100,
            ..Default::default()
        };
        let (release, gate) = std::sync::mpsc::channel();
        let (entered, began) = tokio::sync::oneshot::channel();
        let control = Arc::new(AtomicU8::new(1));
        let original = control.clone();
        let task = tokio::spawn(async move {
            owned
                .run(
                    "Private fixture utterance",
                    &Profile::default(),
                    Ok(&settings),
                    Privacy::Configured,
                    control,
                    move |_| {
                        let _ = entered.send(());
                        let _ = gate.recv();
                        Ok(fotw_secrets::SecretString::new("fixture-token"))
                    },
                )
                .await
        });
        began.await.unwrap();
        match action {
            "cancel" => {
                original.store(2, Ordering::Release);
                assert!(task.await.unwrap().is_err());
            }
            "drop" => {
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
            }
            _ => {
                let result = task.await.unwrap().unwrap();
                assert_eq!(result.text, "Private fixture utterance");
                assert_eq!(result.disposition, Disposition::Fallback);
            }
        }
        assert!(service.in_flight(), "blocked initializer lost ownership");
        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while service.in_flight() {
            assert!(
                std::time::Instant::now() < deadline,
                "released initializer never cleaned up"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            matches!(listener.accept(),Err(ref e) if e.kind()==std::io::ErrorKind::WouldBlock),
            "late credential resolution started a request after revocation"
        );
    }
}
