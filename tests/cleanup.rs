use murmur::cleanup::*;
use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicUsize, Ordering},
    },
    time::Duration,
};
struct Fake {
    calls: AtomicUsize,
    answer: Result<Plan, String>,
    hang: bool,
}
impl Adapter for Fake {
    fn propose<'a>(
        &'a self,
        request: Request<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Plan, String>> + Send + 'a>>
    {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert!(!request.text.is_empty());
            if self.hang {
                std::future::pending::<()>().await;
            }
            self.answer.clone()
        })
    }
}
fn fake(remove: Vec<usize>, breaks: Vec<LineBreak>) -> Fake {
    Fake {
        calls: AtomicUsize::new(0),
        answer: Ok(Plan {
            version: 1,
            remove,
            breaks,
        }),
        hang: false,
    }
}
#[test]
fn versioned_golden_and_adversarial_corpus_preserves_original_tokens() {
    let corpus: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/cleanup/v1.json")).unwrap();
    assert_eq!(corpus["version"], 1);
    for case in corpus["cases"].as_array().unwrap() {
        let plan: Plan = serde_json::from_value(case["plan"].clone()).unwrap();
        let result = apply_plan(
            case["raw"].as_str().unwrap(),
            case["language"].as_str().unwrap(),
            &plan,
        );
        let expected = case["cleaned"].as_str().map(str::to_owned);
        assert_eq!(result, expected, "{}", case["name"]);
    }
}
#[tokio::test]
async fn disabled_offline_and_cancelled_attempts_never_call_the_adapter() {
    for (enabled, privacy, cancelled) in [
        (false, Privacy::Configured, false),
        (true, Privacy::OfflineAll, false),
        (true, Privacy::Configured, true),
    ] {
        let adapter = fake(vec![], vec![]);
        let result = process(
            "private utterance",
            "en",
            enabled,
            privacy,
            Arc::new(AtomicU8::new(if cancelled { 2 } else { 1 })),
            Duration::from_millis(20),
            &adapter,
        )
        .await;
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 0);
        if cancelled {
            assert!(result.is_err());
        } else {
            assert_eq!(result.unwrap().text, "private utterance");
        }
    }
}
#[tokio::test]
async fn stalled_provider_returns_raw_within_the_cleanup_deadline() {
    let mut adapter = fake(vec![], vec![]);
    adapter.hang = true;
    let result = tokio::time::timeout(
        Duration::from_millis(100),
        process(
            "Raw winning transcript",
            "en",
            true,
            Privacy::Configured,
            Arc::new(AtomicU8::new(1)),
            Duration::from_millis(5),
            &adapter,
        ),
    )
    .await;
    let result = result
        .expect("cleanup must not hold raw delivery indefinitely")
        .unwrap();
    assert_eq!(result.text, "Raw winning transcript");
    assert_eq!(result.disposition, Disposition::Fallback);
}
#[tokio::test]
async fn provider_failure_and_invalid_edits_return_the_raw_winning_transcript() {
    for answer in [
        Err("provider response contains sensitive material".into()),
        Ok(Plan {
            version: 1,
            remove: vec![0],
            breaks: vec![],
        }),
    ] {
        let adapter = Fake {
            calls: AtomicUsize::new(0),
            answer,
            hang: false,
        };
        let result = process(
            "Never delete invoice 42",
            "en",
            true,
            Privacy::Configured,
            Arc::new(AtomicU8::new(1)),
            Duration::from_millis(20),
            &adapter,
        )
        .await
        .unwrap();
        assert_eq!(result.text, "Never delete invoice 42");
        assert_eq!(result.disposition, Disposition::Fallback);
    }
}
#[tokio::test]
async fn cancellation_after_request_starts_revokes_even_a_late_success() {
    struct Waiting {
        started: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }
    impl Adapter for Waiting {
        fn propose<'a>(
            &'a self,
            _: Request<'a>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Plan, String>> + Send + 'a>>
        {
            Box::pin(async move {
                self.started.notify_one();
                self.release.notified().await;
                Ok(Plan {
                    version: 1,
                    remove: vec![0],
                    breaks: vec![],
                })
            })
        }
    }
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let adapter = Waiting {
        started: started.clone(),
        release: release.clone(),
    };
    let control = Arc::new(AtomicU8::new(1));
    let cancel = control.clone();
    let task = tokio::spawn(async move {
        process(
            "um keep original",
            "en",
            true,
            Privacy::Configured,
            control,
            Duration::from_secs(1),
            &adapter,
        )
        .await
    });
    started.notified().await;
    cancel.store(2, Ordering::Release);
    release.notify_one();
    assert!(task.await.unwrap().is_err());
}

#[test]
fn formatted_lists_remain_available_locally_and_cannot_relax_remote_paste_policy() {
    let raw = "Milk eggs";
    let formatted = Outcome {
        text: "- Milk\n- eggs".into(),
        disposition: Disposition::Cleaned,
    };
    let local = Outcome {
        text: formatted.text.clone(),
        disposition: Disposition::Cleaned,
    }
    .for_destination(raw, false);
    assert_eq!(local.text, "- Milk\n- eggs");
    let remote = formatted.for_destination(raw, true);
    assert_eq!(remote.text, raw);
    assert_eq!(remote.disposition, Disposition::Fallback);
}
