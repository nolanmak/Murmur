//! Experimental Linux desktop adapter. Audio never touches disk.
use fotw_audio::{
    AudioTap, CaptureTimestamp, FrameFlags, FrameSink, SampleFormat, StreamFormat, TapError, TapId,
};
use serde::Serialize;
use std::{
    collections::HashMap,
    io::{BufRead, Read},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

#[derive(Debug, Serialize)]
pub struct Microphone {
    pub serial: String,
    pub name: String,
}
pub fn microphones_from_json(raw: &str) -> Result<Vec<Microphone>, String> {
    // pw-dump may emit additional JSON arrays when the graph changes while its
    // initial snapshot is being written. Apply these updates, including removal.
    let mut sources = std::collections::BTreeMap::new();
    let mut snapshots = 0;
    for snapshot in serde_json::Deserializer::from_str(raw).into_iter::<serde_json::Value>() {
        let snapshot = snapshot.map_err(|_| "Invalid PipeWire response")?;
        let nodes = snapshot.as_array().ok_or("Invalid PipeWire node list")?;
        snapshots += 1;
        for node in nodes {
            let props = &node["info"]["props"];
            let identity = node["id"]
                .as_u64()
                .map(|id| format!("id:{id}"))
                .or_else(|| {
                    props["object.serial"]
                        .as_u64()
                        .map(|id| format!("serial:{id}"))
                });
            let Some(identity) = identity else {
                continue;
            };
            if node.get("info").is_some_and(serde_json::Value::is_null) {
                sources.remove(&identity);
                continue;
            }
            if !props.is_object() {
                continue;
            }
            sources.remove(&identity);
            let Some(name) = props["node.name"].as_str() else {
                continue;
            };
            if props["media.class"] != "Audio/Source" || name.ends_with(".monitor") {
                continue;
            }
            let Some(serial) = props["object.serial"].as_u64() else {
                continue;
            };
            sources.insert(
                identity,
                Microphone {
                    serial: serial.to_string(),
                    name: props["node.description"].as_str().unwrap_or(name).into(),
                },
            );
        }
    }
    if snapshots == 0 {
        return Err("Empty PipeWire response".into());
    }
    Ok(sources.into_values().collect())
}
pub fn microphones() -> Result<Vec<Microphone>, String> {
    let output = Command::new("timeout")
        .args(["3", "pw-dump"])
        .output()
        .map_err(|_| "Install pipewire-bin and coreutils")?;
    if !output.status.success() {
        return Err("Cannot contact PipeWire; check your desktop audio service".into());
    }
    microphones_from_json(&String::from_utf8_lossy(&output.stdout))
}
pub fn config_path(vars: &HashMap<String, String>) -> Option<PathBuf> {
    vars.get("FOTW_ENV_FILE")
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            vars.get("XDG_CONFIG_HOME")
                .filter(|v| PathBuf::from(v).is_absolute())
                .map(PathBuf::from)
                .or_else(|| vars.get("HOME").map(|h| PathBuf::from(h).join(".config")))
                .map(|p| p.join("murmur/env"))
        })
}
pub fn credentials() -> Result<crate::config::Credentials, String> {
    use std::os::unix::fs::MetadataExt;
    let vars: HashMap<String, String> = std::env::vars().collect();
    let path = config_path(&vars);
    let explicit = vars
        .get("FOTW_ENV_FILE")
        .is_some_and(|v| !v.trim().is_empty());
    let path = path.filter(|p| explicit || p.exists());
    if vars
        .get("DEEPGRAM_API_KEY")
        .is_none_or(|v| v.trim().is_empty())
        && let Some(path) = &path
    {
        let meta = std::fs::metadata(path).map_err(|_| "Cannot read configured credential file")?;
        if !meta.is_file() || meta.mode() & 0o077 != 0 {
            return Err(
                "Credential file must be private: chmod 600 the configured env file".into(),
            );
        }
    }
    crate::config::resolve(&vars, path.as_deref(), || {
        Err(
            "No Deepgram key. Set DEEPGRAM_API_KEY or configure ~/.config/murmur/env (mode 600)."
                .into(),
        )
    })
}
struct PipeWireTap {
    id: TapId,
    serial: String,
    child: Option<Child>,
    reader: Option<JoinHandle<()>>,
    stopping: Arc<AtomicBool>,
}
impl PipeWireTap {
    fn new(serial: String) -> Self {
        Self {
            id: TapId::mic("pipewire"),
            serial,
            child: None,
            reader: None,
            stopping: Arc::new(AtomicBool::new(false)),
        }
    }
}
impl AudioTap for PipeWireTap {
    fn id(&self) -> &TapId {
        &self.id
    }
    fn format(&self) -> StreamFormat {
        StreamFormat::new(48000, 1, SampleFormat::F32)
    }
    fn format_is_authoritative(&self) -> bool {
        true
    }
    fn start(&mut self, mut sink: Box<dyn FrameSink>) -> Result<StreamFormat, TapError> {
        if self.child.is_some() {
            return Err(TapError::AlreadyRunning);
        }
        self.stopping.store(false, Ordering::Release);
        let mut child = Command::new("pw-record")
            .args([
                "--target",
                &self.serial,
                "--rate",
                "48000",
                "--channels",
                "1",
                "--format",
                "f32",
                "--properties",
                "{ node.dont-reconnect = true }",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| TapError::io("starting PipeWire capture", e))?;
        let mut stdout = child.stdout.take().expect("piped stdout");
        let stopping = self.stopping.clone();
        self.child = Some(child);
        self.reader = Some(std::thread::spawn(move || {
            let mut bytes = [0u8; 1920];
            let mut position = 0;
            loop {
                if stdout.read_exact(&mut bytes).is_err() {
                    if !stopping.load(Ordering::Acquire) {
                        sink.on_error(TapError::platform("Microphone disconnected"));
                    }
                    break;
                }
                let pcm: Vec<f32> = bytes
                    .chunks_exact(4)
                    .map(|v| f32::from_le_bytes(v.try_into().unwrap()))
                    .collect();
                sink.on_frames(
                    &pcm,
                    CaptureTimestamp::new(position, fotw_audio::clock::host_ns()),
                    FrameFlags::empty(),
                );
                position += pcm.len() as u64;
            }
        }));
        Ok(self.format())
    }
    fn stop(&mut self) -> Result<(), TapError> {
        self.stopping.store(true, Ordering::Release);
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        Ok(())
    }
}
impl Drop for PipeWireTap {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
fn emit(value: serde_json::Value) {
    println!("{value}");
}
pub fn session(serial: &str) -> Result<(), String> {
    let key = credentials()?;
    if !microphones()?.iter().any(|m| m.serial == serial) {
        return Err("Selected microphone is unavailable; refresh the device list".into());
    }
    let control = Arc::new(AtomicU8::new(0));
    let input_control = control.clone();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            match line.as_deref() {
                Ok("stop") => {
                    let _ =
                        input_control.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
                }
                Ok("cancel") | Err(_) => {
                    input_control.store(2, Ordering::Release);
                    return;
                }
                _ => {}
            }
        }
        input_control.store(2, Ordering::Release);
    });
    let runtime = tokio::runtime::Runtime::new().map_err(|_| "Cannot initialize runtime")?;
    runtime.block_on(async {
        let receiving = Arc::new(AtomicBool::new(false));
        let capture = crate::capture::run_observed(Box::new(PipeWireTap::new(serial.into())), key.key.expose().into(), control.clone(), fotw_stt::DeepgramEndpoint::production(), receiving.clone());
        tokio::pin!(capture);
        let mut tick = tokio::time::interval(Duration::from_millis(20));
        let mut announced = false;
        loop {
            tokio::select! {
                result = &mut capture => {
                    if control.load(Ordering::Acquire) == 2 { emit(serde_json::json!({"state":"cancelled"})); }
                    else { match result { Ok(text) => emit(serde_json::json!({"state":"review", "text":text})), Err(error) => emit(serde_json::json!({"state":"error", "message":error})) } }
                    return Ok(());
                }
                _ = tick.tick() => if !announced && receiving.load(Ordering::Acquire) && control.load(Ordering::Acquire) == 0 { announced = true; emit(serde_json::json!({"state":"recording"})); }
            }
        }
    })
}
pub fn run() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|_| "Cannot locate Murmur executable")?;
    let status = Command::new("python3")
        .args(["-c", include_str!("linux_ui.py")])
        .arg(exe)
        .status()
        .map_err(|_| "Install python3-gi and gir1.2-gtk-3.0")?;
    if status.success() {
        Ok(())
    } else {
        Err("Linux desktop unavailable. Run in a graphical session with GTK 3 and Python GObject installed.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{accept_async, tungstenite::Message};

    fn assert_no_capture_streams() {
        for _ in 0..50 {
            let raw = Command::new("pw-dump").output().unwrap().stdout;
            let mut active = std::collections::BTreeSet::new();
            for batch in serde_json::Deserializer::from_slice(&raw).into_iter::<serde_json::Value>()
            {
                for node in batch.unwrap().as_array().unwrap() {
                    let id = node["id"].as_u64().unwrap();
                    if node["info"].is_null() {
                        active.remove(&id);
                    } else if node["info"]["props"]["media.class"] == "Stream/Input/Audio" {
                        active.insert(id);
                    }
                }
            }
            if active.is_empty() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("Capture stream remained after teardown");
    }

    /// Run only through scripts/test-linux-native.py, which supplies a private
    /// PipeWire server with a synthetic source and no hardware/session manager.
    #[tokio::test]
    #[ignore = "requires the isolated PipeWire fixture runner"]
    async fn native_pipewire_finalizes_and_tears_down() {
        assert_eq!(std::env::var("MURMUR_NATIVE_FIXTURE").as_deref(), Ok("1"));
        let sources = microphones().unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].name, "Murmur synthetic fixture");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let control = Arc::new(AtomicU8::new(0));
        let finish = control.clone();
        let provider = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(socket).await.unwrap();
            let mut chunks = 0;
            while let Some(Ok(message)) = ws.next().await {
                match message {
                    Message::Binary(pcm) => {
                        assert_eq!(pcm.len() % 2, 0);
                        assert!(pcm.iter().any(|v| *v != 0));
                        chunks += 1;
                        if chunks == 3 {
                            finish.store(1, Ordering::Release);
                        }
                    }
                    Message::Text(text) if text.contains("CloseStream") => {
                        assert!(chunks >= 3);
                        ws.send(Message::text(r#"{"type":"Results","is_final":true,"speech_final":true,"channel":{"alternatives":[{"transcript":"Synthetic Linux capture."}]}}"#)).await.unwrap();
                        ws.send(Message::text(r#"{"type":"Metadata","duration":0.2}"#))
                            .await
                            .unwrap();
                        return;
                    }
                    _ => {}
                }
            }
            panic!("Capture ended without CloseStream");
        });
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            crate::capture::run_with_endpoint(
                Box::new(PipeWireTap::new(sources[0].serial.clone())),
                "fixture-key".into(),
                control,
                fotw_stt::DeepgramEndpoint::loopback(port),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result, "Synthetic Linux capture.");
        provider.await.unwrap();
        assert_no_capture_streams();
    }
    #[tokio::test]
    #[ignore = "requires the isolated PipeWire fixture runner"]
    async fn native_pipewire_cancel_discards_late_final_and_releases_input() {
        assert_eq!(std::env::var("MURMUR_NATIVE_FIXTURE").as_deref(), Ok("1"));
        let sources = microphones().unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].name, "Murmur synthetic fixture");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let control = Arc::new(AtomicU8::new(0));
        let cancel = control.clone();
        let provider = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(socket).await.unwrap();
            while let Some(Ok(message)) = ws.next().await {
                if matches!(message, Message::Binary(_)) {
                    cancel.store(2, Ordering::Release);
                    let _ = ws.send(Message::text(r#"{"type":"Results","is_final":true,"speech_final":true,"channel":{"alternatives":[{"transcript":"Must be discarded."}]}}"#)).await;
                    // Remain connected until the client tears down the network.
                    while ws.next().await.is_some_and(|m| m.is_ok()) {}
                    return;
                }
            }
            panic!("No native PCM received");
        });
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            crate::capture::run_with_endpoint(
                Box::new(PipeWireTap::new(sources[0].serial.clone())),
                "fixture-key".into(),
                control,
                fotw_stt::DeepgramEndpoint::loopback(port),
            ),
        )
        .await
        .unwrap();
        assert_eq!(result, Err("Cancelled".into()));
        tokio::time::timeout(Duration::from_secs(2), provider)
            .await
            .unwrap()
            .unwrap();
        assert_no_capture_streams();
    }
}
