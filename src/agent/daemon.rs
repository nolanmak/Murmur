//! `murmur agentd` (#113): owns native Codex/Claude PTYs and serves a Unix
//! socket in a private (0700) directory. Detaching a view never stops the
//! native process; only the agent itself or daemon shutdown ends it.
//!
//! Protocol: one JSON request line per connection, answered by JSON lines.
//! `attach` then switches to binary frames (see [`frame`]).
use super::{Agent, history, reply, terminal};
use portable_pty::{CommandBuilder, PtySize};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

pub const SOCKET: &str = "agents.sock";
const BUFFER: usize = 300_000;

pub struct Config {
    /// Private directory holding the socket.
    pub dir: PathBuf,
    /// Home whose native histories are observed.
    pub home: PathBuf,
    pub codex: PathBuf,
    pub claude: PathBuf,
    pub poll: Duration,
    /// Longest wait for one native reply.
    pub deadline: Duration,
}

impl Config {
    pub fn dir() -> Result<PathBuf, String> {
        Ok(
            PathBuf::from(std::env::var_os("HOME").ok_or("Cannot locate home directory")?)
                .join("Library/Application Support/Murmur/agents"),
        )
    }
    /// The user's installed CLIs; `MURMUR_CODEX`/`MURMUR_CLAUDE` override.
    pub fn system() -> Result<Self, String> {
        let home =
            PathBuf::from(std::env::var_os("HOME").ok_or("Cannot locate home directory")?);
        Ok(Self {
            dir: Self::dir()?,
            codex: binary(&home, "codex", "MURMUR_CODEX"),
            claude: binary(&home, "claude", "MURMUR_CLAUDE"),
            home,
            poll: Duration::from_millis(250),
            deadline: Duration::from_secs(15 * 60),
        })
    }
}

fn binary(home: &Path, name: &str, var: &str) -> PathBuf {
    if let Some(path) = std::env::var_os(var).filter(|v| !v.is_empty()) {
        return PathBuf::from(path);
    }
    let mut candidates = vec![
        home.join(".local/bin").join(name),
        PathBuf::from("/opt/homebrew/bin").join(name),
        PathBuf::from("/usr/local/bin").join(name),
    ];
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|d| d.join(name)));
    }
    candidates
        .into_iter()
        .find(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

/// Binary frames after `attach`: kind byte, big-endian u32 length, payload.
pub mod frame {
    pub const OUTPUT: u8 = 0;
    pub const INPUT: u8 = 1;
    pub const RESIZE: u8 = 2;
    pub const EXIT: u8 = 3;
    pub const MAX: usize = 1 << 20;

    pub fn encode(kind: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(5 + payload.len());
        out.push(kind);
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    pub fn read(reader: &mut impl std::io::Read) -> std::io::Result<(u8, Vec<u8>)> {
        let mut head = [0u8; 5];
        reader.read_exact(&mut head)?;
        let len = u32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize;
        if len > MAX {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "frame too large",
            ));
        }
        let mut payload = vec![0u8; len];
        reader.read_exact(&mut payload)?;
        Ok((head[0], payload))
    }
}

struct Session {
    agent: Agent,
    ids: Mutex<Vec<String>>,
    cwd: PathBuf,
    pid: u32,
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn portable_pty::MasterPty + Send>>,
    child: Mutex<Box<dyn portable_pty::Child + Send + Sync>>,
    buffer: Mutex<VecDeque<u8>>,
    subscribers: Mutex<Vec<mpsc::Sender<Vec<u8>>>>,
    exited: AtomicBool,
    pending: AtomicBool,
    prompt: Mutex<terminal::Prompt>,
    busy: AtomicBool,
    /// Attached views that are real terminals and answer queries themselves.
    terminals: std::sync::atomic::AtomicUsize,
}

impl Session {
    fn id(&self) -> String {
        self.ids.lock().unwrap_or_else(|e| e.into_inner()).last().cloned().unwrap_or_default()
    }
    fn has_id(&self, id: &str) -> bool {
        self.ids.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|i| i == id)
    }
    fn write(&self, bytes: &[u8]) -> Result<(), String> {
        if self.exited.load(Ordering::SeqCst) {
            return Err("This native session has exited".into());
        }
        let mut writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        writer
            .write_all(bytes)
            .and_then(|_| writer.flush())
            .map_err(|_| "Cannot write to the native session".into())
    }
    fn broadcast(&self, frame: Vec<u8>) {
        self.subscribers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|s| s.send(frame.clone()).is_ok());
    }
    fn resize(&self, cols: u16, rows: u16) {
        let _ = self.master.lock().unwrap_or_else(|e| e.into_inner()).resize(PtySize {
            rows: rows.clamp(10, 200),
            cols: cols.clamp(40, 400),
            pixel_width: 0,
            pixel_height: 0,
        });
    }
    fn kill(&self) {
        let _ = self.child.lock().unwrap_or_else(|e| e.into_inner()).kill();
    }
    fn history_path(&self, home: &Path) -> Option<PathBuf> {
        let id = self.id();
        match self.agent {
            Agent::Claude => Some(history::claude_file(home, &self.cwd, &id)),
            Agent::Codex if self.pending.load(Ordering::SeqCst) => None,
            Agent::Codex => history::locate(home, Agent::Codex, &id).ok().flatten(),
        }
    }
}

#[derive(Default)]
struct State {
    sessions: Vec<Arc<Session>>,
    active: Option<Arc<Session>>,
}

struct Daemon {
    config: Config,
    state: Mutex<State>,
    stop: AtomicBool,
}

pub struct Handle {
    daemon: Arc<Daemon>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Handle {
    pub fn shutdown(mut self) {
        self.stop();
    }
    fn stop(&mut self) {
        self.daemon.stop.store(true, Ordering::SeqCst);
        for session in self.daemon.state.lock().unwrap_or_else(|e| e.into_inner()).sessions.iter() {
            session.kill();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(self.daemon.config.dir.join(SOCKET));
    }
    /// Blocks until the daemon stops.
    pub fn wait(mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        if self.thread.is_some() {
            self.stop();
        }
    }
}

fn private_dir(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    std::fs::create_dir_all(dir).map_err(|_| "Cannot create the agent socket directory")?;
    let meta = std::fs::symlink_metadata(dir).map_err(|_| "Cannot inspect the agent directory")?;
    if !meta.is_dir() || meta.uid() != rustix::process::getuid().as_raw() {
        return Err("The agent socket directory is not a private directory you own".into());
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|_| "Cannot secure the agent socket directory".into())
}

pub fn start(config: Config) -> Result<Handle, String> {
    use std::os::unix::fs::PermissionsExt;
    private_dir(&config.dir)?;
    let path = config.dir.join(SOCKET);
    // sockaddr_un holds 104 bytes on macOS (108 on Linux), including the NUL.
    if path.as_os_str().len() > 100 {
        return Err("The agent socket path is too long for a Unix socket".into());
    }
    if UnixStream::connect(&path).is_ok() {
        return Err("Murmur agent daemon is already running".into());
    }
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).map_err(|_| "Cannot open the agent socket")?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .map_err(|_| "Cannot secure the agent socket")?;
    listener
        .set_nonblocking(true)
        .map_err(|_| "Cannot configure the agent socket")?;
    let daemon = Arc::new(Daemon {
        config,
        state: Mutex::new(State::default()),
        stop: AtomicBool::new(false),
    });
    let worker = daemon.clone();
    let thread = std::thread::spawn(move || {
        while !worker.stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let daemon = worker.clone();
                    std::thread::spawn(move || {
                        let _ = stream.set_nonblocking(false);
                        daemon.connection(stream);
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        }
    });
    Ok(Handle {
        daemon,
        thread: Some(thread),
    })
}

fn line(stream: &mut UnixStream, value: Value) -> std::io::Result<()> {
    stream.write_all(format!("{value}\n").as_bytes())
}

fn error(stream: &mut UnixStream, message: &str) {
    let _ = line(stream, json!({"type": "error", "error": message}));
}

fn agent(value: &Value) -> Result<Agent, String> {
    match value.get("agent").and_then(Value::as_str) {
        Some("codex") => Ok(Agent::Codex),
        Some("claude") => Ok(Agent::Claude),
        _ => Err("Use codex or claude".into()),
    }
}

fn size(value: &Value) -> (u16, u16) {
    let get = |k: &str, d: u64| value.get(k).and_then(Value::as_u64).unwrap_or(d).min(1000) as u16;
    (get("cols", 100), get("rows", 30))
}

impl Daemon {
    fn connection(self: &Arc<Self>, mut stream: UnixStream) {
        let mut reader = BufReader::new(match stream.try_clone() {
            Ok(s) => s,
            Err(_) => return,
        });
        let mut request = String::new();
        if reader.by_ref().take(64 * 1024).read_line(&mut request).is_err() {
            return;
        }
        let Ok(request) = serde_json::from_str::<Value>(&request) else {
            return error(&mut stream, "Invalid request");
        };
        let result = match request.get("op").and_then(Value::as_str) {
            Some("open") => self.open(&request).map(|id| json!({"type": "ok", "id": id})),
            Some("list") => Ok(json!({"type": "ok", "sessions": self.list()})),
            Some("attach") => return self.attach(&request, stream, reader),
            Some("send") => return self.send(&request, stream),
            Some("shutdown") => {
                for session in self.state.lock().unwrap_or_else(|e| e.into_inner()).sessions.iter() {
                    session.kill();
                }
                self.stop.store(true, Ordering::SeqCst);
                let _ = std::fs::remove_file(self.config.dir.join(SOCKET));
                Ok(json!({"type": "ok"}))
            }
            _ => Err("Unknown request".into()),
        };
        match result {
            Ok(value) => {
                let _ = line(&mut stream, value);
            }
            Err(message) => error(&mut stream, &message),
        }
    }

    fn find(&self, id: Option<&str>) -> Result<Arc<Session>, String> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match id {
            Some(id) => state
                .sessions
                .iter()
                .find(|s| s.has_id(id))
                .cloned()
                .ok_or_else(|| "No Murmur agent session has that id".into()),
            None => state
                .active
                .clone()
                .filter(|s| !s.exited.load(Ordering::SeqCst))
                .or_else(|| {
                    state
                        .sessions
                        .iter()
                        .rev()
                        .find(|s| !s.exited.load(Ordering::SeqCst))
                        .cloned()
                })
                .ok_or_else(|| {
                    "No live Murmur agent session. Start one with: murmur agent run codex (or claude)"
                        .into()
                }),
        }
    }

    fn activate(&self, session: &Arc<Session>) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).active = Some(session.clone());
    }

    fn list(&self) -> Vec<Value> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .sessions
            .iter()
            .map(|s| {
                json!({
                    "agent": s.agent.name(),
                    "id": s.id(),
                    "cwd": s.cwd.to_string_lossy(),
                    "live": !s.exited.load(Ordering::SeqCst),
                    "active": state.active.as_ref().is_some_and(|a| Arc::ptr_eq(a, s)),
                })
            })
            .collect()
    }

    fn open(self: &Arc<Self>, request: &Value) -> Result<String, String> {
        let agent = agent(request)?;
        let cwd = PathBuf::from(request.get("cwd").and_then(Value::as_str).unwrap_or(""));
        if !cwd.is_absolute() {
            return Err("Select an absolute project directory".into());
        }
        let cwd = cwd
            .canonicalize()
            .ok()
            .filter(|p| p.is_dir())
            .ok_or("Project directory does not exist")?;
        let resume = match request.get("resume").and_then(Value::as_str) {
            Some(id) if history::is_uuid(id) => Some(id.to_string()),
            Some(_) => return Err("Resume needs a native session UUID".into()),
            None => None,
        };
        if let Some(id) = &resume
            && let Ok(existing) = self.find(Some(id))
            && !existing.exited.load(Ordering::SeqCst)
        {
            self.activate(&existing);
            return Ok(existing.id());
        }
        let (program, args, id, pending) = match agent {
            Agent::Claude => {
                let id = match &resume {
                    Some(id) => id.clone(),
                    None => terminal::new_uuid()?,
                };
                let flag = if resume.is_some() { "--resume" } else { "--session-id" };
                (&self.config.claude, vec![flag.to_string(), id.clone()], id, false)
            }
            Agent::Codex => {
                let mut args = Vec::new();
                if let Some(id) = &resume {
                    args.extend(["resume".to_string(), id.clone()]);
                }
                args.extend([
                    "--no-alt-screen".to_string(),
                    "-c".into(),
                    "check_for_update_on_startup=false".into(),
                ]);
                match &resume {
                    Some(id) => (&self.config.codex, args, id.clone(), false),
                    // Codex picks its thread id; this one is provisional.
                    None => (&self.config.codex, args, terminal::new_uuid()?, true),
                }
            }
        };
        let (cols, rows) = size(request);
        let pair = portable_pty::native_pty_system()
            .openpty(PtySize {
                rows: rows.clamp(10, 200),
                cols: cols.clamp(40, 400),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|_| "Cannot open a terminal for the agent")?;
        let mut command = CommandBuilder::new(program);
        command.args(&args);
        command.cwd(&cwd);
        // The native CLI writes its history under the home Murmur observes.
        command.env("HOME", &self.config.home);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        for (name, _) in std::env::vars_os() {
            if name.to_str().is_some_and(terminal::inherited_session_marker) {
                command.env_remove(&name);
            }
        }
        let mut path = vec![
            self.config.home.join(".local/bin"),
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ];
        path.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin:/usr/sbin:/sbin".into()),
        ));
        if let Ok(path) = std::env::join_paths(path) {
            command.env("PATH", path);
        }
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|_| format!("Cannot start {}; is it installed?", agent.name()))?;
        drop(pair.slave);
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|_| "Cannot read the agent terminal")?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|_| "Cannot write the agent terminal")?;
        let session = Arc::new(Session {
            agent,
            ids: Mutex::new(vec![id.clone()]),
            cwd,
            pid: child.process_id().unwrap_or(0),
            writer: Mutex::new(writer),
            master: Mutex::new(pair.master),
            child: Mutex::new(child),
            buffer: Mutex::new(VecDeque::new()),
            subscribers: Mutex::new(Vec::new()),
            exited: AtomicBool::new(false),
            pending: AtomicBool::new(pending),
            prompt: Mutex::new(terminal::Prompt::default()),
            busy: AtomicBool::new(false),
            terminals: std::sync::atomic::AtomicUsize::new(0),
        });
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.sessions.push(session.clone());
            state.active = Some(session.clone());
        }
        let output = session.clone();
        std::thread::spawn(move || pump(output, reader));
        if pending {
            let resolving = session.clone();
            std::thread::spawn(move || {
                while !resolving.exited.load(Ordering::SeqCst) {
                    if let Some(real) = terminal::codex_thread_from_lsof(&terminal::lsof(resolving.pid)) {
                        resolving.ids.lock().unwrap_or_else(|e| e.into_inner()).push(real);
                        resolving.pending.store(false, Ordering::SeqCst);
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
            });
        }
        Ok(id)
    }

    fn attach(self: &Arc<Self>, request: &Value, mut stream: UnixStream, mut reader: BufReader<UnixStream>) {
        let id = request.get("id").and_then(Value::as_str);
        let session = match id.ok_or_else(|| "Provide a session id".to_string()).and_then(|id| self.find(Some(id))) {
            Ok(s) => s,
            Err(message) => return error(&mut stream, &message),
        };
        if line(&mut stream, json!({"type": "ok", "id": session.id()})).is_err() {
            return;
        }
        self.activate(&session);
        let (cols, rows) = size(request);
        session.resize(cols, rows);
        let tty = request.get("tty").and_then(Value::as_bool).unwrap_or(false);
        if tty {
            session.terminals.fetch_add(1, Ordering::SeqCst);
        }
        let (sender, receiver) = mpsc::channel::<Vec<u8>>();
        {
            // Snapshot and subscribe under the buffer lock so no output is
            // lost or duplicated between the replay and the live stream.
            let buffer = session.buffer.lock().unwrap_or_else(|e| e.into_inner());
            let replay: Vec<u8> = buffer.iter().copied().collect();
            let _ = sender.send(frame::encode(frame::OUTPUT, &replay));
            if session.exited.load(Ordering::SeqCst) {
                let _ = sender.send(frame::encode(frame::EXIT, &0i32.to_be_bytes()));
            } else {
                session.subscribers.lock().unwrap_or_else(|e| e.into_inner()).push(sender);
            }
        }
        let mut out = match stream.try_clone() {
            Ok(s) => s,
            Err(_) => return,
        };
        std::thread::spawn(move || {
            for frame in receiver {
                if out.write_all(&frame).is_err() {
                    break;
                }
            }
            let _ = out.shutdown(std::net::Shutdown::Both);
        });
        while let Ok((kind, payload)) = frame::read(&mut reader) {
            match kind {
                frame::INPUT => {
                    session.prompt.lock().unwrap_or_else(|e| e.into_inner()).typed(&payload);
                    if session.write(&payload).is_err() {
                        break;
                    }
                }
                frame::RESIZE if payload.len() == 4 => session.resize(
                    u16::from_be_bytes([payload[0], payload[1]]),
                    u16::from_be_bytes([payload[2], payload[3]]),
                ),
                _ => {}
            }
        }
        // Client detached or crashed: the native session keeps running. The
        // sender is dropped from subscribers on its next failed write.
        if tty {
            session.terminals.fetch_sub(1, Ordering::SeqCst);
        }
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }

    fn send(self: &Arc<Self>, request: &Value, mut stream: UnixStream) {
        let id = request.get("id").and_then(Value::as_str);
        let text = request.get("text").and_then(Value::as_str).unwrap_or("");
        let result = self.find(id).and_then(|session| {
            if session.exited.load(Ordering::SeqCst) {
                return Err("This native session has exited".to_string());
            }
            let bytes = terminal::voice_bytes(text)?;
            if session.prompt.lock().unwrap_or_else(|e| e.into_inner()).draft() {
                return Err("There is unfinished text in the terminal prompt. Send or clear it before speaking.".into());
            }
            if session.busy.swap(true, Ordering::SeqCst) {
                return Err("This session is already answering a voice turn".into());
            }
            Ok((session, bytes))
        });
        let (session, bytes) = match result {
            Ok(v) => v,
            Err(message) => return error(&mut stream, &message),
        };
        self.activate(&session);
        let outcome = self.turn(&session, text.trim(), &bytes, &mut stream);
        session.busy.store(false, Ordering::SeqCst);
        let _ = match outcome {
            Ok(reply) => line(&mut stream, json!({"type": "done", "reply": reply})),
            Err(message) => line(&mut stream, json!({"type": "error", "error": message})),
        };
    }

    fn turn(&self, session: &Arc<Session>, text: &str, bytes: &[u8], stream: &mut UnixStream) -> Result<String, String> {
        let home = &self.config.home;
        let mut path = session.history_path(home);
        let mut offset = path
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .unwrap_or(0);
        // Watch the screen while pasting: Enter goes only to an input box
        // that visibly took the paste, never to a native dialog.
        let (sender, screen) = mpsc::channel::<Vec<u8>>();
        session.subscribers.lock().unwrap_or_else(|e| e.into_inner()).push(sender);
        session.write(bytes)?;
        let mut shown = Vec::new();
        let echo_deadline = Instant::now() + Duration::from_secs(2);
        while !terminal::paste_echoed(&shown, text) {
            let left = echo_deadline.saturating_duration_since(Instant::now());
            match screen.recv_timeout(left.max(Duration::from_millis(1))) {
                Ok(frame) if frame.first() == Some(&frame::OUTPUT) => shown.extend_from_slice(&frame[5..]),
                Ok(_) => {}
                Err(_) if Instant::now() >= echo_deadline => {
                    drop(screen);
                    return Err(format!(
                        "The agent is showing a prompt or dialog, so Murmur did not press Enter. Answer it in the terminal: murmur agent run {} --resume {}",
                        session.agent.name(),
                        session.id()
                    ));
                }
                Err(_) => {}
            }
        }
        drop(screen);
        session.write(b"\r")?;
        let _ = line(stream, json!({"type": "session", "id": session.id()}));
        let mut watcher = reply::Watcher::new(session.agent, text);
        let mut reply = String::new();
        let deadline = Instant::now() + self.config.deadline;
        while Instant::now() < deadline && !self.stop.load(Ordering::SeqCst) {
            if path.is_none() {
                path = session.history_path(home);
            }
            if let Some(p) = &path
                && let Ok(mut file) = std::fs::File::open(p)
            {
                let size = file.metadata().map(|m| m.len()).unwrap_or(0);
                if size < offset {
                    offset = 0;
                }
                if size > offset {
                    use std::io::{Seek, SeekFrom};
                    let mut bytes = Vec::new();
                    if file.seek(SeekFrom::Start(offset)).is_ok()
                        && file.take((size - offset).min(1 << 20)).read_to_end(&mut bytes).is_ok()
                    {
                        offset += bytes.len() as u64;
                        for event in watcher.feed_bytes(&bytes) {
                            match event {
                                reply::Event::Text(text) => {
                                    if line(stream, json!({"type": "text", "text": text})).is_err() {
                                        // The client went away; the native turn continues.
                                        return Err("Client disconnected".into());
                                    }
                                    reply.push_str(&text);
                                    reply.push('\n');
                                }
                                reply::Event::Done => return Ok(reply),
                                reply::Event::Started => {}
                            }
                        }
                        continue;
                    }
                }
            }
            if session.exited.load(Ordering::SeqCst) {
                return Err("The native session exited before replying".into());
            }
            std::thread::sleep(self.config.poll);
        }
        Err("No completed turn received. Check the terminal for a permission prompt or login message.".into())
    }
}

/// Copies PTY output into the replay buffer and to attached views.
fn pump(session: Arc<Session>, mut reader: Box<dyn Read + Send>) {
    let mut chunk = [0u8; 8192];
    loop {
        let n = match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let data = &chunk[..n];
        {
            let mut buffer = session.buffer.lock().unwrap_or_else(|e| e.into_inner());
            buffer.extend(data);
            let excess = buffer.len().saturating_sub(BUFFER);
            buffer.drain(..excess);
            session.broadcast(frame::encode(frame::OUTPUT, data));
        }
        // Native TUIs (Codex) wait at startup for terminal answers. With no
        // real terminal attached, answer them like a dark xterm would.
        if session.terminals.load(Ordering::SeqCst) == 0 {
            let answers = terminal::query_answers(data);
            if !answers.is_empty() {
                let _ = session.write(&answers);
            }
        }
    }
    let code = loop {
        let status = session.child.lock().unwrap_or_else(|e| e.into_inner()).try_wait();
        match status {
            Ok(Some(status)) => break status.exit_code() as i32,
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => break -1,
        }
    };
    let _buffer = session.buffer.lock().unwrap_or_else(|e| e.into_inner());
    session.exited.store(true, Ordering::SeqCst);
    session.broadcast(frame::encode(frame::EXIT, &code.to_be_bytes()));
    session.subscribers.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

