//! Cleanup completes through the existing generation and target guards, with no native input.
use murmur::{
    cleanup::{Adapter, Plan, Privacy, Request, process},
    core::{CompletionEffect, Dictation},
    insertion::{Target, allowed},
};
use std::{
    sync::{Arc, atomic::AtomicU8},
    time::Duration,
};
struct Waiting {
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
impl Adapter for Waiting {
    fn propose<'a>(
        &'a self,
        request: Request<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Plan, String>> + Send + 'a>>
    {
        Box::pin(async move {
            assert_eq!(request.text, "um keep invoice 42");
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
#[tokio::test]
async fn a_late_cleanup_cannot_reach_a_new_generation_or_changed_field() {
    for changed_generation in [true, false] {
        let mut core = Dictation::default();
        assert!(core.start_manual());
        let generation = core.generation();
        assert!(core.stop());
        let before = Target {
            pid: 42,
            element: 7,
            secure: false,
            editable: true,
        };
        let mut after = before.clone();
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let adapter = Waiting {
            started: started.clone(),
            release: release.clone(),
        };
        let task = tokio::spawn(async move {
            process(
                "um keep invoice 42",
                "en",
                true,
                Privacy::Configured,
                Arc::new(AtomicU8::new(1)),
                Duration::from_secs(5),
                &adapter,
            )
            .await
        });
        started.notified().await;
        if changed_generation {
            assert!(core.cancel());
            assert!(core.start_manual());
            assert!(core.stop());
        } else {
            after.element = 8;
        }
        release.notify_one();
        let result = task.await.unwrap().unwrap();
        if changed_generation {
            assert_eq!(core.complete(generation, Ok(result.text)), None);
            let next = core.generation();
            assert_eq!(
                core.complete(next, Ok("new field text".into())),
                Some(CompletionEffect::Transcript("new field text".into()))
            );
        } else {
            let Some(CompletionEffect::Transcript(text)) =
                core.complete(generation, Ok(result.text))
            else {
                panic!("valid current completion expected")
            };
            assert!(
                !allowed(&before, &after, &text),
                "changed field must refuse the cleaned completion"
            );
        }
    }
}
