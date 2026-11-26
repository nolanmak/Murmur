//! #112: `murmur mcp` keeps Agentflow's published tool contract.
use murmur::agent::{Agent, mcp};
use serde_json::{Value, json};
use std::sync::Mutex;

#[derive(Default)]
struct Fake {
    spoken: Mutex<Vec<(String, Option<f32>)>>,
    fail: bool,
}
impl mcp::Tools for Fake {
    fn speak(&self, text: &str, speed: Option<f32>) -> Result<(), String> {
        if self.fail {
            return Err("No audio output".into());
        }
        self.spoken.lock().unwrap().push((text.into(), speed));
        Ok(())
    }
    fn now(&self) -> Value {
        json!({"utc": "2026-11-26T12:00:00Z", "local": "fixture", "timezone": "America/Los_Angeles"})
    }
    fn history(&self, agent: Agent, id: &str) -> Result<Value, String> {
        Ok(json!({"agent": agent.name(), "id": id, "messages": []}))
    }
}

fn call(tools: &Fake, request: Value) -> Value {
    serde_json::from_str(&mcp::handle(&request.to_string(), tools).expect("a response")).unwrap()
}

#[test]
fn initialize_and_tools_list_match_the_agentflow_contract() {
    let fake = Fake::default();
    let init = call(
        &fake,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}),
    );
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "murmur");
    assert!(init["result"]["capabilities"]["tools"].is_object());
    let list = call(&fake, json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["speak", "current_time", "session_history"]);
    let speak = &list["result"]["tools"][0]["inputSchema"];
    assert_eq!(speak["required"], json!(["text"]));
    assert_eq!(speak["properties"]["text"]["maxLength"], 12000);
    assert_eq!(
        speak["properties"]["speed"]["enum"],
        json!([0.75, 1, 1.25, 1.5, 1.75, 2])
    );
    assert_eq!(
        call(&fake, json!({"jsonrpc":"2.0","id":3,"method":"ping"}))["result"],
        json!({})
    );
}

#[test]
fn speak_plays_text_and_reports_failures_as_tool_errors() {
    let fake = Fake::default();
    let ok = call(
        &fake,
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"speak","arguments":{"text":"Done.","speed":2}}}),
    );
    assert_eq!(
        ok["result"]["content"][0]["text"],
        "Speech played on the local Mac."
    );
    assert_eq!(
        *fake.spoken.lock().unwrap(),
        vec![("Done.".to_string(), Some(2.0))]
    );
    let bad_speed = call(
        &fake,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"speak","arguments":{"text":"x","speed":3}}}),
    );
    assert_eq!(bad_speed["result"]["isError"], true);
    let failing = Fake {
        fail: true,
        ..Default::default()
    };
    let error = call(
        &failing,
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"speak","arguments":{"text":"x"}}}),
    );
    assert_eq!(error["result"]["isError"], true);
    assert_eq!(error["result"]["content"][0]["text"], "No audio output");
}

#[test]
fn time_and_history_tools_validate_their_arguments() {
    let fake = Fake::default();
    let time = call(
        &fake,
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"current_time","arguments":{}}}),
    );
    let body: Value =
        serde_json::from_str(time["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(body["timezone"], "America/Los_Angeles");
    let ok = call(
        &fake,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"session_history","arguments":{"agent":"claude","id":"3f0c6f3e-9a3b-4c55-8d1e-0b6a3c2d1e0f"}}}),
    );
    assert!(ok["result"]["isError"].is_null());
    for args in [
        json!({"agent":"gemini","id":"3f0c6f3e-9a3b-4c55-8d1e-0b6a3c2d1e0f"}),
        json!({"agent":"codex","id":"../x"}),
        json!({"agent":"codex"}),
    ] {
        let r = call(
            &fake,
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"session_history","arguments":args}}),
        );
        assert_eq!(r["result"]["isError"], true, "{args}");
    }
}

#[test]
fn notifications_and_garbage_get_no_reply_and_unknown_methods_error() {
    let fake = Fake::default();
    assert!(mcp::handle("not json", &fake).is_none());
    assert!(
        mcp::handle(
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            &fake
        )
        .is_none()
    );
    let unknown = call(
        &fake,
        json!({"jsonrpc":"2.0","id":9,"method":"resources/list"}),
    );
    assert_eq!(unknown["error"]["code"], -32601);
}

#[test]
fn current_time_reports_utc_and_a_timezone() {
    let now = mcp::current_time();
    let utc = now["utc"].as_str().unwrap();
    assert!(utc.ends_with('Z') && utc.len() == 20, "{utc}");
    assert!(now["timezone"].as_str().is_some_and(|tz| !tz.is_empty()));
}

#[test]
fn unsupported_protocol_versions_get_a_supported_one() {
    let fake = Fake::default();
    for (asked, want) in [
        ("2024-11-05", "2024-11-05"),
        ("2025-06-18", "2025-06-18"),
        ("1900-01-01", mcp::LATEST_PROTOCOL),
    ] {
        let init = call(
            &fake,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":asked}}),
        );
        assert_eq!(init["result"]["protocolVersion"], want, "{asked}");
    }
}

/// A speak that blocks until released, to prove the reader stays live.
struct Slow {
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}
impl mcp::Tools for Slow {
    fn speak(&self, _: &str, _: Option<f32>) -> Result<(), String> {
        let _ = self
            .release
            .lock()
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(5));
        Ok(())
    }
    fn now(&self) -> Value {
        json!({})
    }
    fn history(&self, _: Agent, _: &str) -> Result<Value, String> {
        Ok(json!({}))
    }
}

#[test]
fn pings_are_answered_while_speech_is_playing() {
    let (release, wait) = std::sync::mpsc::channel();
    let tools = std::sync::Arc::new(Slow {
        release: std::sync::Mutex::new(wait),
    });
    let (read_in, mut write_in) = std::io::pipe().unwrap();
    let (read_out, write_out) = std::io::pipe().unwrap();
    let server =
        std::thread::spawn(move || mcp::serve(std::io::BufReader::new(read_in), write_out, tools));
    use std::io::{BufRead, Write};
    writeln!(
        write_in,
        r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"speak","arguments":{{"text":"long"}}}}}}"#
    )
    .unwrap();
    writeln!(write_in, r#"{{"jsonrpc":"2.0","id":2,"method":"ping"}}"#).unwrap();
    let mut lines = std::io::BufReader::new(read_out).lines();
    let first: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
    assert_eq!(first["id"], 2, "ping waited for speech: {first}");
    release.send(()).unwrap();
    let second: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
    assert_eq!(second["id"], 1);
    drop(write_in);
    server.join().unwrap().unwrap();
}
