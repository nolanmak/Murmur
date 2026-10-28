//! Murmur #101: the real `SpeechdBackend` against FlyOnTheWall's model-free
//! fake helper (`fotw-speechd-fake`, from the pinned FlyOnTheWall revision).
//! No model, Swift, microphone or network: the fake is built offline from the
//! dependency source Cargo already fetched, with this repository's lockfile.
use murmur::local_parakeet::{Endpoint, Service, SpeechdBackend};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU8;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

fn fake_helper() -> PathBuf {
    static FAKE: OnceLock<PathBuf> = OnceLock::new();
    FAKE.get_or_init(build_fake).clone()
}

fn build_fake() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    // The pinned revision, from the lockfile this build used.
    let lock = std::fs::read_to_string(root.join("Cargo.lock")).unwrap();
    let rev = lock
        .split("[[package]]")
        .find(|block| block.contains("name = \"fotw-speechd-client\""))
        .and_then(|block| block.split("rev=").nth(1))
        .and_then(|rest| rest.split('#').next())
        .expect("fotw-speechd-client is pinned to a git revision")
        .to_owned();
    // Cargo checked that revision out to build this test; find it without
    // `cargo metadata`, which would want every platform's packages offline.
    let cargo_home = std::env::var_os("CARGO_HOME").map_or_else(
        || PathBuf::from(std::env::var_os("HOME").expect("HOME")).join(".cargo"),
        PathBuf::from,
    );
    let fake_source = std::fs::read_dir(cargo_home.join("git/checkouts"))
        .expect("Cargo git checkouts")
        .filter_map(Result::ok)
        .map(|repo| {
            repo.path()
                .join(&rev[..7])
                .join("crates/fotw-speechd-client/src/bin/fotw-speechd-fake.rs")
        })
        .find(|path| path.is_file())
        .expect("the pinned FlyOnTheWall checkout");
    let target =
        std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from);
    let package = target.join("speechd-fake");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("Cargo.toml"),
        format!(
            r#"[package]
name = "speechd-fake"
version = "0.0.0"
edition = "2024"
publish = false

[workspace]

[dependencies]
fotw-speechd-client = {{ git = "https://github.com/nolanmak/FlyOnTheWall", rev = "{rev}" }}
serde_json = "1"

[[bin]]
name = "fotw-speechd-fake"
path = {fake_source:?}
"#
        ),
    )
    .unwrap();
    // Murmur's lockfile resolves every dependency the fake has, offline.
    std::fs::copy(root.join("Cargo.lock"), package.join("Cargo.lock")).unwrap();
    let status = std::process::Command::new(&cargo)
        .args([
            "build",
            "--offline",
            "--quiet",
            "--bin",
            "fotw-speechd-fake",
        ])
        .arg("--manifest-path")
        .arg(package.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(package.join("target"))
        .status()
        .expect("cargo build");
    assert!(status.success(), "building the fake helper failed");
    package.join("target/debug/fotw-speechd-fake")
}

fn endpoint(models: &Path) -> Endpoint {
    let config = fotw_speechd_client::SpeechdConfig::new(fake_helper(), models);
    let backend = SpeechdBackend::new(Ok(config), "fake-batch", Some("fake-vad"));
    Endpoint::new(Arc::new(Service::new(Arc::new(backend)).unwrap()))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_helper_gates_trims_and_transcribes_one_batch_per_utterance() {
    let models = tempfile::tempdir().unwrap();
    let endpoint = endpoint(models.path());
    let ready = endpoint
        .prepare("en", Duration::from_secs(10))
        .await
        .expect("fake engines load and warm");

    // 1 s of quiet, 0.5 s of "speech" (|x| >= 16384 is speech to fake-vad),
    // then 1 s of digital zero.
    let mut pcm = vec![0i16; 16_000];
    pcm.extend(std::iter::repeat_n(20_000i16, 8_000));
    pcm.extend(std::iter::repeat_n(0i16, 16_000));
    let text = ready
        .transcribe(&pcm, Arc::new(AtomicU8::new(0)), Duration::from_secs(20))
        .await
        .unwrap();
    let samples: usize = text
        .strip_prefix("samples=")
        .unwrap_or_else(|| panic!("one batch final: {text:?}"))
        .parse()
        .unwrap();
    assert!(
        samples >= 8_000 && samples < pcm.len(),
        "only the speech and its padding reach the recognizer: {samples} of {}",
        pcm.len()
    );

    // Not speech to the gate: no batch stream at all, so no 2.5 s fake-batch
    // wait, and nothing to type.
    let started = Instant::now();
    let quiet = vec![100i16; 32_000];
    let text = ready
        .transcribe(&quiet, Arc::new(AtomicU8::new(0)), Duration::from_secs(20))
        .await
        .unwrap();
    assert_eq!(text, "");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_helper_that_lacks_the_engine_is_refused_before_recording() {
    let models = tempfile::tempdir().unwrap();
    let config = fotw_speechd_client::SpeechdConfig::new(fake_helper(), models.path());
    let backend = SpeechdBackend::new(Ok(config), "parakeet-unified", Some("silero-vad"));
    let endpoint = Endpoint::new(Arc::new(Service::new(Arc::new(backend)).unwrap()));
    let error = endpoint
        .prepare("en", Duration::from_secs(10))
        .await
        .unwrap_err();
    assert!(error.contains("does not offer parakeet-unified"), "{error}");
}

#[tokio::test]
async fn a_build_without_the_helper_explains_how_to_get_it() {
    let backend = SpeechdBackend::new(Err("no helper in this build".into()), "x", None);
    let endpoint = Endpoint::new(Arc::new(Service::new(Arc::new(backend)).unwrap()));
    let error = endpoint
        .prepare("en", Duration::from_secs(2))
        .await
        .unwrap_err();
    assert!(error.contains("no helper in this build"), "{error}");
}
