//! `murmur mcp` (#112): Agentflow's stdio MCP tools on Murmur's speech stack.
use super::{Agent, history, speech};
use serde_json::{Value, json};
use std::io::{BufRead, Write};

pub const LATEST_PROTOCOL: &str = "2025-06-18";
const SUPPORTED_PROTOCOLS: [&str; 3] = ["2024-11-05", "2025-03-26", LATEST_PROTOCOL];

pub trait Tools {
    fn speak(&self, text: &str, speed: Option<f32>) -> Result<(), String>;
    fn now(&self) -> Value;
    fn history(&self, agent: Agent, id: &str) -> Result<Value, String>;
}

fn tool_list() -> Value {
    json!([
        {
            "name": "speak",
            "description": "Read text aloud on the user’s Mac when they request a spoken response. Optionally set speed for this one response; use 2 for twice normal playback. This tool does not change or resume the agent session.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": {"type": "string", "minLength": 1, "maxLength": speech::MAX_TEXT},
                    "speed": {
                        "type": "number",
                        "enum": [0.75, 1, 1.25, 1.5, 1.75, 2],
                        "description": "Optional playback speed for this response only. 1 is normal and 2 is twice normal."
                    }
                },
                "required": ["text"],
                "additionalProperties": false
            }
        },
        {
            "name": "current_time",
            "description": "Read the Mac’s current date, time, and IANA timezone. Use for time-sensitive questions; never guess the current time.",
            "annotations": {"readOnlyHint": true},
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
        },
        {
            "name": "session_history",
            "description": "Read the selected native session’s most recent messages with original timestamps. A bounded history preview, not a replacement for native context.",
            "annotations": {"readOnlyHint": true},
            "inputSchema": {
                "type": "object",
                "properties": {"agent": {"enum": ["codex", "claude"]}, "id": {"type": "string"}},
                "required": ["agent", "id"],
                "additionalProperties": false
            }
        }
    ])
}

fn call(name: &str, args: &Value, tools: &dyn Tools) -> Result<String, String> {
    match name {
        "speak" => {
            let text = args
                .get("text")
                .and_then(Value::as_str)
                .ok_or("Provide text to speak")?;
            speech::validate_text(text)?;
            let speed = match args.get("speed") {
                None | Some(Value::Null) => None,
                Some(v) => Some(speech::speed(v.as_f64().ok_or("Speed must be a number")?)?),
            };
            tools.speak(text, speed)?;
            Ok("Speech played on the local Mac.".into())
        }
        "current_time" => Ok(tools.now().to_string()),
        "session_history" => {
            let agent = match args.get("agent").and_then(Value::as_str) {
                Some("codex") => Agent::Codex,
                Some("claude") => Agent::Claude,
                _ => return Err("Provide the native agent and session UUID".into()),
            };
            let id = args
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| history::is_uuid(id))
                .ok_or("Provide the native agent and session UUID")?;
            Ok(tools.history(agent, id)?.to_string())
        }
        _ => Err("Unknown tool".into()),
    }
}

/// One JSON-RPC line in, at most one line out. Notifications and
/// unparseable lines get no reply.
pub fn handle(line: &str, tools: &dyn Tools) -> Option<String> {
    let message: Value = serde_json::from_str(line).ok()?;
    let id = message.get("id")?.clone();
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let reply = match method {
        "initialize" => json!({"result": {
            "protocolVersion": params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .filter(|v| SUPPORTED_PROTOCOLS.contains(v))
                .unwrap_or(LATEST_PROTOCOL),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "murmur", "version": env!("CARGO_PKG_VERSION")}
        }}),
        "tools/list" => json!({"result": {"tools": tool_list()}}),
        "ping" => json!({"result": {}}),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match call(name, &args, tools) {
                Ok(text) => json!({"result": {"content": [{"type": "text", "text": text}]}}),
                Err(error) => {
                    json!({"result": {"isError": true, "content": [{"type": "text", "text": error}]}})
                }
            }
        }
        _ => json!({"error": {"code": -32601, "message": "Method not found"}}),
    };
    let mut reply = reply;
    reply["jsonrpc"] = json!("2.0");
    reply["id"] = id;
    Some(reply.to_string())
}

/// Reads requests until EOF. Tool calls run on their own threads so pings
/// and other requests are answered while speech plays; replies are written
/// whole, one per line.
pub fn serve(
    input: impl BufRead,
    output: impl Write + Send + 'static,
    tools: std::sync::Arc<dyn Tools + Send + Sync>,
) -> Result<(), String> {
    let output = std::sync::Arc::new(std::sync::Mutex::new(output));
    let mut calls = Vec::new();
    for line in input.lines() {
        let line = line.map_err(|_| "Cannot read MCP input")?;
        let is_call = serde_json::from_str::<Value>(&line)
            .ok()
            .is_some_and(|m| m.get("method").and_then(Value::as_str) == Some("tools/call"));
        if is_call {
            let (tools, output) = (tools.clone(), output.clone());
            calls.push(std::thread::spawn(move || {
                if let Some(reply) = handle(&line, tools.as_ref()) {
                    let mut out = output.lock().unwrap_or_else(|e| e.into_inner());
                    let _ = writeln!(out, "{reply}").and_then(|_| out.flush());
                }
            }));
        } else if let Some(reply) = handle(&line, tools.as_ref()) {
            let mut out = output.lock().unwrap_or_else(|e| e.into_inner());
            writeln!(out, "{reply}")
                .and_then(|_| out.flush())
                .map_err(|_| "Cannot write MCP output")?;
        }
        calls.retain(|c| !c.is_finished());
    }
    for call in calls {
        let _ = call.join();
    }
    Ok(())
}

/// Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

fn timezone() -> String {
    if let Some(tz) = std::env::var("TZ").ok().filter(|t| !t.is_empty()) {
        return tz.trim_start_matches(':').to_string();
    }
    std::fs::read_link("/etc/localtime")
        .ok()
        .and_then(|target| {
            let target = target.to_string_lossy().into_owned();
            target
                .split_once("zoneinfo/")
                .map(|(_, zone)| zone.to_string())
        })
        .unwrap_or_else(|| "UTC".into())
}

pub fn current_time() -> Value {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (y, m, d) = civil(secs.div_euclid(86_400));
    let s = secs.rem_euclid(86_400);
    let utc = format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        s / 3600,
        s / 60 % 60,
        s % 60
    );
    let local = std::process::Command::new("/bin/date")
        .arg("+%Y-%m-%dT%H:%M:%S%z %Z")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| utc.clone());
    json!({"utc": utc, "local": local, "timezone": timezone()})
}

/// Real tools: Murmur's configured voice and the user's native histories.
pub struct System {
    voice: speech::Voicebox,
}
impl Default for System {
    fn default() -> Self {
        Self {
            voice: speech::Voicebox::system(),
        }
    }
}
impl Tools for System {
    fn speak(&self, text: &str, speed: Option<f32>) -> Result<(), String> {
        self.voice.speak(text, speed).map(|_| ())
    }
    fn now(&self) -> Value {
        current_time()
    }
    fn history(&self, agent: Agent, id: &str) -> Result<Value, String> {
        let home = std::path::PathBuf::from(
            std::env::var_os("HOME").ok_or("Cannot locate home directory")?,
        );
        let messages = match history::locate(&home, agent, id)? {
            Some(path) => history::read(&path, agent)?,
            None => Vec::new(),
        };
        Ok(json!({"agent": agent.name(), "id": id, "messages": messages}))
    }
}
