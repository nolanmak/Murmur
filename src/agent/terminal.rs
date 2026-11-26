//! Terminal input rules for Murmur-owned native sessions (#113).

/// Tracks whether the native prompt holds text the user typed but did not
/// send, so voice never merges into or submits someone's draft. Errs toward
/// "draft": a false draft only asks the user to clear the prompt.
#[derive(Default)]
pub struct Prompt {
    draft: bool,
    backslash: bool,
}
impl Prompt {
    pub fn typed(&mut self, bytes: &[u8]) {
        const RECALL: [&[u8]; 4] = [b"\x1b[A", b"\x1b[B", b"\x1bOA", b"\x1bOB"];
        if bytes == b"\x1b\r" || bytes.starts_with(b"\x1b[200~") || RECALL.contains(&bytes) {
            // Option+Enter newline, a terminal paste, or history recall.
            self.draft = true;
            self.backslash = false;
            return;
        }
        if bytes.first() == Some(&0x1b) {
            return;
        }
        for &b in bytes {
            match b {
                // Backslash-Enter inserts a newline instead of sending.
                b'\r' | b'\n' => self.draft = self.backslash,
                0x03 | 0x15 => self.draft = false,
                0x7f | 0x08 => {}
                _ => self.draft = true,
            }
            self.backslash = b == b'\\';
        }
    }
    /// Text may be sitting in the prompt (a refused voice paste).
    pub fn mark_draft(&mut self) {
        self.draft = true;
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

fn find_queries(output: &[u8]) -> Vec<(usize, usize, &'static [u8])> {
    let mut found = Vec::new();
    for (query, answer) in QUERIES {
        let mut at = 0;
        while let Some(i) = output[at..].windows(query.len()).position(|w| w == query) {
            found.push((at + i, query.len(), answer));
            at += i + query.len();
        }
    }
    found.sort_by_key(|(i, _, _)| *i);
    found
}

/// Answers for the terminal queries in `output`, in order: cursor position
/// (DSR), device attributes (DA1) and foreground/background color (OSC 10/11).
pub fn query_answers(output: &[u8]) -> Vec<u8> {
    find_queries(output)
        .into_iter()
        .flat_map(|(_, _, answer)| answer.to_vec())
        .collect()
}

/// Recent output to replay on attach. Old terminal queries are removed (the
/// attaching terminal would answer them as typed input), and a buffer cut
/// mid-stream starts at the next full line.
pub fn replay(buffer: &[u8], trimmed: bool) -> Vec<u8> {
    let start = if trimmed {
        buffer
            .iter()
            .position(|b| *b == b'\n')
            .map_or(buffer.len(), |i| i + 1)
    } else {
        0
    };
    let buffer = &buffer[start..];
    let mut out = Vec::with_capacity(buffer.len());
    let mut at = 0;
    for (i, len, _) in find_queries(buffer) {
        if i >= at {
            out.extend_from_slice(&buffer[at..i]);
            at = i + len;
        }
    }
    out.extend_from_slice(&buffer[at..]);
    out
}

/// A virtual terminal screen of a native session, used to tell whether a
/// voice paste reached the input box.
pub struct Screen {
    parser: vt100::Parser,
}

impl Screen {
    pub fn new(rows: u16, cols: u16) -> Self {
        let size =
            |n: u16| std::num::NonZeroU16::new(n.max(1)).unwrap_or(std::num::NonZeroU16::MIN);
        Self {
            parser: vt100::Parser::new(size(rows), size(cols), 0),
        }
    }
    pub fn process(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }
    pub fn resize(&mut self, rows: u16, cols: u16) {
        if let (Some(rows), Some(cols)) = (
            std::num::NonZeroU16::new(rows),
            std::num::NonZeroU16::new(cols),
        ) {
            self.parser.set_size(rows, cols);
        }
    }
    pub fn rows(&self) -> Vec<String> {
        let (_, cols) = self.parser.screen().size();
        self.parser.screen().rows(0, cols.get()).collect()
    }

    /// True when some screen row (or a row wrapped into the next) shows the
    /// start of `text`, or a collapsed-paste placeholder, and did not show
    /// it before the paste. A dialog repaint or older transcript text that
    /// already contained the words is not an echo.
    pub fn newly_shows(before: &[String], after: &[String], text: &str) -> bool {
        let norm = |s: &str| -> String {
            s.chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect()
        };
        let probe: String = norm(text).chars().take(16).collect();
        let probes: Vec<&str> = [probe.as_str(), "pastedtext", "pastedcontent"]
            .into_iter()
            .filter(|p| !p.is_empty())
            .collect();
        let joined = |rows: &[String], i: usize| -> String {
            let mut row = rows.get(i).map(|r| norm(r)).unwrap_or_default();
            row.push_str(&rows.get(i + 1).map(|r| norm(r)).unwrap_or_default());
            row
        };
        (0..after.len()).any(|i| {
            let now = joined(after, i);
            let was = joined(before, i);
            probes.iter().any(|p| now.contains(p) && !was.contains(p))
        })
    }
}
