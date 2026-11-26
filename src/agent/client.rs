//! Client side of the agent socket (#113): CLI, menu bar and tests.
use super::{
    Agent,
    daemon::{SOCKET, frame},
};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::Path,
    sync::mpsc,
    time::{Duration, Instant},
};

pub enum Frame {
    Output(Vec<u8>),
    Exit(i32),
}

#[derive(Debug, Clone)]
pub struct Session {
    pub agent: String,
    pub id: String,
    pub cwd: String,
    pub live: bool,
    pub active: bool,
}

fn connect(dir: &Path) -> Result<UnixStream, String> {
    UnixStream::connect(dir.join(SOCKET))
        .map_err(|_| "Murmur's agent daemon is not running. Start a session with: murmur agent run codex (or claude)".into())
}

fn request(dir: &Path, body: Value) -> Result<(UnixStream, BufReader<UnixStream>), String> {
    let mut stream = connect(dir)?;
    stream
        .write_all(format!("{body}\n").as_bytes())
        .map_err(|_| "Cannot reach Murmur's agent daemon")?;
    let reader = BufReader::new(
        stream
            .try_clone()
            .map_err(|_| "Cannot reach Murmur's agent daemon")?,
    );
    Ok((stream, reader))
}

fn next(reader: &mut BufReader<UnixStream>) -> Result<Value, String> {
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) | Err(_) => Err("Murmur's agent daemon closed the connection".into()),
        Ok(_) => serde_json::from_str(&line).map_err(|_| "Invalid daemon reply".into()),
    }
}

fn ok(value: Value) -> Result<Value, String> {
    if value.get("type").and_then(Value::as_str) == Some("error") {
        return Err(value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("Agent daemon error")
            .to_string());
    }
    Ok(value)
}

pub fn open(
    dir: &Path,
    agent: Agent,
    resume: Option<&str>,
    cwd: &Path,
    cols: u16,
    rows: u16,
) -> Result<String, String> {
    let (_, mut reader) = request(
        dir,
        json!({"op": "open", "agent": agent.name(), "resume": resume, "cwd": cwd.to_string_lossy(), "cols": cols, "rows": rows}),
    )?;
    let reply = ok(next(&mut reader)?)?;
    reply
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "Invalid daemon reply".into())
}

pub fn list(dir: &Path) -> Result<Vec<Session>, String> {
    let (_, mut reader) = request(dir, json!({"op": "list"}))?;
    let reply = ok(next(&mut reader)?)?;
    Ok(reply
        .get("sessions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|s| Session {
            agent: s["agent"].as_str().unwrap_or("").into(),
            id: s["id"].as_str().unwrap_or("").into(),
            cwd: s["cwd"].as_str().unwrap_or("").into(),
            live: s["live"].as_bool().unwrap_or(false),
            active: s["active"].as_bool().unwrap_or(false),
        })
        .collect())
}

/// Ends every Murmur-owned session and stops the daemon.
pub fn shutdown(dir: &Path) -> Result<(), String> {
    let (_, mut reader) = request(dir, json!({"op": "shutdown"}))?;
    ok(next(&mut reader)?).map(|_| ())
}

/// Sends one voice turn and streams the native reply text to `on_text`.
pub fn send(
    dir: &Path,
    id: Option<&str>,
    text: &str,
    mut on_text: impl FnMut(&str),
) -> Result<String, String> {
    let (_stream, mut reader) = request(dir, json!({"op": "send", "id": id, "text": text}))?;
    loop {
        let event = ok(next(&mut reader)?)?;
        match event.get("type").and_then(Value::as_str) {
            Some("text") => on_text(event["text"].as_str().unwrap_or("")),
            Some("done") => return Ok(event["reply"].as_str().unwrap_or("").to_string()),
            _ => {}
        }
    }
}

pub struct Attachment {
    stream: UnixStream,
    frames: mpsc::Receiver<Frame>,
}

impl Attachment {
    /// The next output or exit frame, or `None` after `timeout`.
    pub fn read(&mut self, timeout: Duration) -> Result<Option<Frame>, String> {
        match self.frames.recv_timeout(timeout) {
            Ok(frame) => Ok(Some(frame)),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err("Detached".into()),
        }
    }
    pub fn input(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.stream
            .write_all(&frame::encode(frame::INPUT, bytes))
            .map_err(|_| "Detached".into())
    }
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), String> {
        let mut payload = cols.to_be_bytes().to_vec();
        payload.extend_from_slice(&rows.to_be_bytes());
        self.stream
            .write_all(&frame::encode(frame::RESIZE, &payload))
            .map_err(|_| "Detached".into())
    }
}

impl Drop for Attachment {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

/// `tty`: the client is a real terminal that answers terminal queries
/// itself; otherwise the daemon answers them.
pub fn attach(dir: &Path, id: &str, cols: u16, rows: u16, tty: bool) -> Result<Attachment, String> {
    let (stream, mut reader) = request(
        dir,
        json!({"op": "attach", "id": id, "cols": cols, "rows": rows, "tty": tty}),
    )?;
    ok(next(&mut reader)?)?;
    let (sender, frames) = mpsc::channel();
    std::thread::spawn(move || {
        while let Ok((kind, payload)) = frame::read(&mut reader) {
            let frame = match kind {
                frame::OUTPUT => Frame::Output(payload),
                frame::EXIT if payload.len() == 4 => Frame::Exit(i32::from_be_bytes([
                    payload[0], payload[1], payload[2], payload[3],
                ])),
                _ => continue,
            };
            if sender.send(frame).is_err() {
                break;
            }
        }
    });
    Ok(Attachment { stream, frames })
}

/// Connects, starting `murmur agentd` in the background if needed.
pub fn ensure_daemon(dir: &Path) -> Result<(), String> {
    if connect(dir).is_ok() {
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|_| "Cannot locate the murmur executable")?;
    std::fs::create_dir_all(dir).map_err(|_| "Cannot create the agent directory")?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("agentd.log"))
        .map_err(|_| "Cannot open the agent daemon log")?;
    use std::os::unix::process::CommandExt;
    std::process::Command::new(exe)
        .arg("agentd")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log)
        .process_group(0)
        .spawn()
        .map_err(|_| "Cannot start Murmur's agent daemon")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if connect(dir).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err("Murmur's agent daemon did not start; see agentd.log".into())
}
