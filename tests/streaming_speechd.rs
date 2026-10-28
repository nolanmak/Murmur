//! Murmur #100: the real `SpeechdBackend` and stream loop against
//! FlyOnTheWall's model-free helper (`fotw-speechd-fake`, `fake-dictation`
//! engine). No model, Swift, microphone or network: the fake is compiled
//! offline from the FlyOnTheWall source Cargo already checked out for this
//! build, resolved with this repository's lockfile.
use murmur::fallback::Progress;
use murmur::local_streaming::{Endpoint, Service, SpeechdBackend};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU8;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// The fake helper, built once per test run.
fn fake_helper() -> PathBuf {
    static FAKE: OnceLock<PathBuf> = OnceLock::new();
    FAKE.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let lock = std::fs::read_to_string(root.join("Cargo.lock")).unwrap();
        // fotw-speechd-client's pinned revision, from the lockfile.
        let block = lock
            .split("[[package]]")
            .find(|b| b.contains("name = \"fotw-speechd-client\""))
            .expect("fotw-speechd-client is locked");
        let rev = block
            .split('#')
            .nth(1)
            .and_then(|r| r.split('"').next())
            .expect("a git revision")
            .to_owned();
        let home = std::env::var_os("CARGO_HOME").map_or_else(
            || PathBuf::from(std::env::var_os("HOME").unwrap()).join(".cargo"),
            PathBuf::from,
        );
        let source = std::fs::read_dir(home.join("git/checkouts"))
            .unwrap()
            .filter_map(Result::ok)
            .map(|repo| {
                repo.path()
                    .join(&rev[..7])
                    .join("crates/fotw-speechd-client/src/bin/fotw-speechd-fake.rs")
            })
            .find(|p| p.is_file())
            .expect("Cargo's checkout of the pinned FlyOnTheWall revision");
        let target = std::env::var_os("CARGO_TARGET_DIR")
            .map_or_else(|| root.join("target"), PathBuf::from)
            .join("streaming-fake");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(
            target.join("Cargo.toml"),
            format!(
                "[package]\nname = \"streaming-fake\"\nversion = \"0.0.0\"\nedition = \"2024\"\npublish = false\n\n[workspace]\n\n[dependencies]\nfotw-speechd-client = {{ git = \"https://github.com/nolanmak/FlyOnTheWall\", rev = \"{rev}\" }}\nserde_json = \"1\"\n\n[[bin]]\nname = \"fotw-speechd-fake\"\npath = {source:?}\n"
            ),
        )
        .unwrap();
        std::fs::copy(root.join("Cargo.lock"), target.join("Cargo.lock")).unwrap();
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let status = std::process::Command::new(cargo)
            .args(["build", "--offline", "--quiet", "--bin", "fotw-speechd-fake"])
            .arg("--manifest-path")
            .arg(target.join("Cargo.toml"))
            .env("CARGO_TARGET_DIR", target.join("target"))
            .status()
            .unwrap();
        assert!(status.success(), "building fotw-speechd-fake offline");
        target.join("target/debug/fotw-speechd-fake")
    })
    .clone()
}

fn endpoint() -> Endpoint {
    let config = fotw_speechd_client::SpeechdConfig::new(fake_helper(), std::env::temp_dir())
        .arg("--fake-engines");
    let backend = SpeechdBackend::new(Ok(config), "fake-dictation");
    let service = Arc::new(Service::new(Arc::new(backend)).unwrap());
    service.warm();
    Endpoint::new(service)
}

async fn until_live(progress: &Progress, want: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while progress.live().as_deref() != Some(want) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("partial {want:?}; last {:?}", progress.live()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_helper_streams_partials_and_the_flushed_final_is_returned() {
    let endpoint = endpoint()
        .prepare("en", Duration::from_secs(10))
        .await
        .expect("fake-dictation is loaded and warm");
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    let progress = Progress::default();
    let control = Arc::new(AtomicU8::new(0));
    let run = tokio::spawn({
        let progress = progress.clone();
        async move { endpoint.transcribe(rx, progress, control).await }
    });
    tx.send(vec![100; 4_000]).await.unwrap();
    until_live(&progress, "steps=1 lang=en").await;
    tx.send(vec![100; 1_000]).await.unwrap();
    drop(tx); // release
    let text = run.await.unwrap().expect("a final");
    assert_eq!(text, "steps=2 lang=en", "the last partial step was flushed");
    assert_eq!(
        progress.live().as_deref(),
        Some("steps=1 lang=en"),
        "the final is not shown as a partial"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_dictation_leaves_the_next_one_a_fresh_stream() {
    let endpoint = endpoint()
        .prepare("en", Duration::from_secs(10))
        .await
        .unwrap();
    // First dictation: Esc mid-stream drops the attempt.
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    let first = Progress::default();
    let run = tokio::spawn({
        let (endpoint, first) = (endpoint.clone(), first.clone());
        async move {
            endpoint
                .transcribe(rx, first, Arc::new(AtomicU8::new(0)))
                .await
        }
    });
    tx.send(vec![100; 4_000]).await.unwrap();
    until_live(&first, "steps=1 lang=en").await;
    tx.send(vec![100; 3_000]).await.unwrap();
    run.abort();
    let _ = run.await;
    // Second dictation, right away: its steps count from zero.
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    let second = Progress::default();
    let run = tokio::spawn({
        let second = second.clone();
        async move {
            endpoint
                .transcribe(rx, second, Arc::new(AtomicU8::new(0)))
                .await
        }
    });
    tx.send(vec![100; 4_000]).await.unwrap();
    drop(tx);
    assert_eq!(run.await.unwrap().expect("final"), "steps=1 lang=en");
    assert_eq!(first.live().as_deref(), Some("steps=1 lang=en"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_engine_error_mid_stream_keeps_every_sample_for_the_retry() {
    let endpoint = endpoint()
        .prepare("en", Duration::from_secs(10))
        .await
        .unwrap();
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    let progress = Progress::default();
    let run = tokio::spawn({
        let progress = progress.clone();
        async move {
            endpoint
                .transcribe(rx, progress, Arc::new(AtomicU8::new(0)))
                .await
        }
    });
    tx.send(vec![100; 4_000]).await.unwrap();
    until_live(&progress, "steps=1 lang=en").await;
    tx.send(vec![i16::MIN; 10]).await.unwrap(); // the fake's failure trigger
    tx.send(vec![100; 2_000]).await.unwrap(); // still held after the failure
    drop(tx);
    let failure = run.await.unwrap().expect_err("the stream failed");
    assert!(
        failure.reason.contains("told to fail"),
        "{}",
        failure.reason
    );
    assert_eq!(failure.pcm.len(), 6_010, "every captured sample is kept");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unsupported_language_is_refused_before_recording() {
    let error = endpoint()
        .prepare("fr", Duration::from_secs(10))
        .await
        .unwrap_err();
    assert!(error.contains("does not transcribe"), "{error}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn leaving_streaming_unloads_the_engine_in_the_helper_and_it_reloads_on_return() {
    let endpoint = endpoint();
    let ready = endpoint
        .prepare("en", Duration::from_secs(10))
        .await
        .unwrap();
    // A finished dictation, so no stream holds the engine.
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    let run = tokio::spawn({
        let ready = ready.clone();
        async move {
            ready
                .transcribe(rx, Progress::default(), Arc::new(AtomicU8::new(0)))
                .await
        }
    });
    tx.send(vec![100; 4_000]).await.unwrap();
    drop(tx);
    run.await.unwrap().expect("final");
    endpoint
        .service()
        .unload()
        .await
        .expect("the helper unloads it");
    assert!(endpoint.service().state().is_none());
    // Choosing streaming again loads it again; a stream works.
    let again = endpoint
        .prepare("en", Duration::from_secs(10))
        .await
        .expect("reloaded");
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    let run = tokio::spawn(async move {
        again
            .transcribe(rx, Progress::default(), Arc::new(AtomicU8::new(0)))
            .await
    });
    tx.send(vec![100; 4_000]).await.unwrap();
    drop(tx);
    assert_eq!(run.await.unwrap().expect("final"), "steps=1 lang=en");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_model_installed_after_the_helper_started_loads_without_a_restart() {
    // The CLI (another process) installs the model while the app's helper is
    // running. The helper's startup capabilities still say "not installed";
    // the next load must ask the helper, which reads the models on disk.
    let root = std::env::temp_dir().join(format!(
        "murmur-streaming-install-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let config = fotw_speechd_client::SpeechdConfig::new(fake_helper(), &root)
        .arg("--fake-engines")
        .arg("--pinned")
        .arg("nemotron-3.5@r1");
    let service =
        Arc::new(Service::new(Arc::new(SpeechdBackend::new(Ok(config), "nemotron-3.5"))).unwrap());
    let missing = service.ready(Duration::from_secs(10)).await.unwrap_err();
    assert!(missing.contains("not installed"), "{missing}");
    // As `murmur speech install-streaming` lays it out, from elsewhere.
    std::fs::create_dir_all(root.join("nemotron-3.5/r1")).unwrap();
    let binding = service
        .ready(Duration::from_secs(10))
        .await
        .expect("the running helper loads the newly installed model");
    assert_eq!(binding.revision, "r1");
    let _ = std::fs::remove_dir_all(root);
}
