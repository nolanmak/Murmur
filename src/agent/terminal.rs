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

/// Environment variables that tie a process to the agent session that
/// launched it. A Murmur-owned session is a new top-level session, so these
/// are removed (Claude stops saving its transcript when it sees them).
/// User configuration such as `CLAUDE_CODE_USE_BEDROCK` is kept.
pub fn inherited_session_marker(name: &str) -> bool {
    matches!(
        name,
        "CLAUDECODE"
            | "CLAUDE_PID"
            | "CLAUDE_CODE_ENTRYPOINT"
            | "CLAUDE_CODE_SESSION_ID"
            | "CLAUDE_CODE_SESSION_ATTENDED"
            | "CLAUDE_CODE_CHILD_SESSION"
            | "CLAUDE_CODE_CHILD_NOTES_DIR"
            | "CLAUDE_CODE_MESSAGING_SOCKET"
            | "CLAUDE_CODE_MESSAGING_TOKEN"
            | "CLAUDE_CODE_BRIDGE_SESSION_ID"
            | "CLAUDE_CODE_EXECPATH"
            | "CODEX_THREAD_ID"
    )
}

/// Answers for the terminal queries in `output`, in order: cursor position
/// (DSR), device attributes (DA1) and foreground/background color (OSC 10/11).
pub fn query_answers(output: &[u8]) -> Vec<u8> {
    const QUERIES: [(&[u8], &[u8]); 9] = [
        (b"\x1b[6n", b"\x1b[1;1R"),
        (b"\x1b[c", b"\x1b[?1;2c"),
        (b"\x1b[0c", b"\x1b[?1;2c"),
        (b"\x1b]10;?\x1b\\", b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\"),
        (b"\x1b]10;?\x07", b"\x1b]10;rgb:ffff/ffff/ffff\x07"),
        (b"\x1b]11;?\x1b\\", b"\x1b]11;rgb:0000/0000/0000\x1b\\"),
        (b"\x1b]11;?\x07", b"\x1b]11;rgb:0000/0000/0000\x07"),
        (b"\x1b]4;0;?\x1b\\", b"\x1b]4;0;rgb:0000/0000/0000\x1b\\"),
        (b"\x1b]4;0;?\x07", b"\x1b]4;0;rgb:0000/0000/0000\x07"),
    ];
    let mut found: Vec<(usize, &[u8])> = Vec::new();
    for (query, answer) in QUERIES {
        let mut at = 0;
        while let Some(i) = output[at..].windows(query.len()).position(|w| w == query) {
            found.push((at + i, answer));
            at += i + query.len();
        }
    }
    found.sort_by_key(|(i, _)| *i);
    found.into_iter().flat_map(|(_, a)| a.to_vec()).collect()
}

/// Removes CSI and OSC escape sequences.
fn strip_escapes(output: &[u8]) -> String {
    let text = String::from_utf8_lossy(output);
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' || (c == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Whether the native input box visibly received a voice paste. Enter is
/// pressed only then: a dialog (permission, model choice) does not echo the
/// paste, and Enter there would answer it.
pub fn paste_echoed(output: &[u8], text: &str) -> bool {
    let shown: String = strip_escapes(output)
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    let probe: String = text
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .take(16)
        .collect();
    (!probe.is_empty() && shown.contains(&probe))
        || shown.contains("pastedtext")
        || shown.contains("pastedcontent")
}
