//! Read-only previews of native Codex/Claude histories (#112). Bounded to
//! the last 1 MB and 40 messages; never writes or follows symlinks.
use super::Agent;
use serde::Serialize;
use serde_json::Value;
use std::{
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

pub const TAIL_BYTES: u64 = 1024 * 1024;
pub const MAX_MESSAGES: usize = 40;

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Message {
    pub role: String,
    pub text: String,
    pub timestamp: Option<String>,
    pub id: Option<String>,
}

pub fn is_uuid(id: &str) -> bool {
    id.len() == 36
        && id.chars().enumerate().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == '-'
            } else {
                c.is_ascii_hexdigit() && !c.is_ascii_uppercase()
            }
        })
}

fn text_blocks(content: &Value, kinds: &[&str]) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter(|x| {
                x.get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|t| kinds.contains(&t))
            })
            .filter_map(|x| x.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn string(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// One native JSONL record as a user/assistant message, if it is one.
pub fn message(record: &Value, agent: Agent) -> Option<Message> {
    match agent {
        Agent::Codex => {
            let payload = record.get("payload")?;
            if record.get("type")? != "response_item" || payload.get("type")? != "message" {
                return None;
            }
            let role = payload.get("role")?.as_str()?;
            if role != "user" && role != "assistant" {
                return None;
            }
            let text = text_blocks(
                payload.get("content")?,
                &["input_text", "output_text", "text"],
            );
            (!text.is_empty()).then(|| Message {
                role: role.into(),
                text,
                timestamp: string(record.get("timestamp")),
                id: string(record.get("ordinal")).or_else(|| string(record.get("timestamp"))),
            })
        }
        Agent::Claude => {
            let role = record.get("type")?.as_str()?;
            if role != "user" && role != "assistant" {
                return None;
            }
            let text = text_blocks(record.get("message")?.get("content")?, &["text"]);
            (!text.is_empty()).then(|| Message {
                role: role.into(),
                text,
                timestamp: string(record.get("timestamp")),
                id: string(record.get("uuid")),
            })
        }
    }
}

/// `truncated`: the bytes start mid-file, so the first line may be cut.
pub fn parse(bytes: &[u8], agent: Agent, truncated: bool) -> Vec<Message> {
    let text = String::from_utf8_lossy(bytes);
    let mut lines = text.split('\n');
    if truncated {
        lines.next();
    }
    let mut messages: Vec<Message> = lines
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|record| message(&record, agent))
        .collect();
    let excess = messages.len().saturating_sub(MAX_MESSAGES);
    messages.drain(..excess);
    messages
}

pub fn read(path: &Path, agent: Agent) -> Result<Vec<Message>, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Never follow a symlink swapped in after lookup.
        options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err("Cannot read native session history".into()),
    };
    let meta = file
        .metadata()
        .map_err(|_| "Cannot read native session history")?;
    if !meta.is_file() || !owned(&meta) {
        return Err("Native session history is not a regular file you own".into());
    }
    let size = meta.len();
    let start = size.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start))
        .map_err(|_| "Cannot read native session history")?;
    let mut bytes = Vec::new();
    file.take(TAIL_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read native session history")?;
    Ok(parse(&bytes, agent, start > 0))
}

fn owned(meta: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.uid() == rustix::process::getuid().as_raw()
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        true
    }
}

/// A regular file owned by this user; symlinks are refused.
fn owned_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    meta.file_type().is_file() && owned(&meta)
}

fn dirs(path: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(path) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    // Newest Codex date folders first.
    dirs.sort_unstable_by(|a, b| b.cmp(a));
    dirs
}

/// Finds a native session file by UUID under `home`.
pub fn locate(home: &Path, agent: Agent, id: &str) -> Result<Option<PathBuf>, String> {
    if !is_uuid(id) {
        return Err("Provide the native agent and session UUID".into());
    }
    match agent {
        Agent::Codex => {
            let suffix = format!("-{id}.jsonl");
            for year in dirs(&home.join(".codex/sessions")) {
                for month in dirs(&year) {
                    for day in dirs(&month) {
                        let Ok(entries) = std::fs::read_dir(&day) else {
                            continue;
                        };
                        for entry in entries.filter_map(Result::ok) {
                            let name = entry.file_name();
                            let name = name.to_string_lossy();
                            if name.starts_with("rollout-")
                                && name.ends_with(&suffix)
                                && owned_file(&entry.path())
                            {
                                return Ok(Some(entry.path()));
                            }
                        }
                    }
                }
            }
            Ok(None)
        }
        Agent::Claude => Ok(dirs(&home.join(".claude/projects"))
            .into_iter()
            .map(|project| project.join(format!("{id}.jsonl")))
            .find(|path| owned_file(path))),
    }
}

/// The Claude project file for a session launched in `cwd`.
pub fn claude_file(home: &Path, cwd: &Path, id: &str) -> PathBuf {
    let mangled: String = cwd
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    home.join(".claude/projects")
        .join(mangled)
        .join(format!("{id}.jsonl"))
}
