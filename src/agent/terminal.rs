//! Terminal input rules for Murmur-owned native sessions (#113).

/// Tracks whether the native prompt holds text the user typed but did not
/// send, so voice never merges into or submits someone's draft.
#[derive(Default)]
pub struct Prompt {
    draft: bool,
}
impl Prompt {
    pub fn typed(&mut self, bytes: &[u8]) {
        if bytes.iter().any(|b| matches!(b, b'\r' | b'\n' | 0x03 | 0x15)) {
            self.draft = false;
        } else if !bytes.is_empty() && bytes[0] != 0x1b {
            self.draft = true;
        }
    }
    pub fn draft(&self) -> bool {
        self.draft
    }
}

/// Voice text as one bracketed paste; Enter is sent separately afterwards.
pub fn voice_bytes(text: &str) -> Result<Vec<u8>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Nothing to send".into());
    }
    if text
        .chars()
        .any(|c| (c.is_control() && c != '\n' && c != '\t') || c == '\u{7f}')
    {
        return Err("Message contains terminal control characters".into());
    }
    let mut bytes = b"\x1b[200~".to_vec();
    bytes.extend_from_slice(text.as_bytes());
    bytes.extend_from_slice(b"\x1b[201~");
    Ok(bytes)
}

/// The thread id from `lsof -Fn` output: Codex holds
/// `thread-writer-locks/<uuid>.lock` open for the thread it writes.
pub fn codex_thread_from_lsof(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let path = line.strip_prefix('n')?;
        let (_, name) = path.rsplit_once("/thread-writer-locks/")?;
        let id = name.strip_suffix(".lock")?;
        super::history::is_uuid(id).then(|| id.to_string())
    })
}

/// Runs lsof for one process; empty when lsof is missing or fails.
pub fn lsof(pid: u32) -> String {
    for program in ["/usr/sbin/lsof", "/usr/bin/lsof"] {
        if let Ok(out) = std::process::Command::new(program)
            .args(["-p", &pid.to_string(), "-Fn"])
            .stderr(std::process::Stdio::null())
            .output()
        {
            return String::from_utf8_lossy(&out.stdout).into_owned();
        }
    }
    String::new()
}

/// A random (v4) UUID for a new Claude session.
pub fn new_uuid() -> Result<String, String> {
    use std::io::Read;
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .map_err(|_| "Cannot create a session id")?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}
