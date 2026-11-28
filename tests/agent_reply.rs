//! #113: reply observation from native JSONL. Synthetic records only.
use murmur::agent::{
    Agent,
    reply::{Event, Watcher},
};
use serde_json::json;

fn feed(w: &mut Watcher, records: &[serde_json::Value]) -> Vec<Event> {
    records.iter().flat_map(|r| w.feed(r)).collect()
}

#[test]
fn codex_reply_starts_at_the_submitted_turn_and_ends_at_task_complete() {
    let mut w = Watcher::new(Agent::Codex, "run the tests");
    let events = feed(
        &mut w,
        &[
            // An earlier turn's answer is not this reply.
            json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"old"}]}}),
            json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"run the tests"}]}}),
            json!({"type":"response_item","payload":{"type":"reasoning","summary":[]}}),
            json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Running."}]}}),
            json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"All green."}]}}),
            json!({"type":"event_msg","payload":{"type":"task_complete"}}),
            json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"late"}]}}),
        ],
    );
    assert_eq!(
        events,
        vec![
            Event::Started,
            Event::Text("Running.".into()),
            Event::Text("All green.".into()),
            Event::Done
        ]
    );
    assert!(w.done());
}

#[test]
fn claude_reply_ends_at_end_turn_or_turn_duration() {
    let mut w = Watcher::new(Agent::Claude, "hello");
    let events = feed(
        &mut w,
        &[
            json!({"type":"user","message":{"role":"user","content":"hello"}}),
            json!({"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash"}],"stop_reason":"tool_use"}}),
            json!({"type":"user","message":{"content":[{"type":"tool_result","content":"x"}]}}),
            json!({"type":"assistant","message":{"content":[{"type":"text","text":"Hi."}],"stop_reason":"end_turn"}}),
        ],
    );
    assert_eq!(
        events,
        vec![Event::Started, Event::Text("Hi.".into()), Event::Done]
    );
    let mut w = Watcher::new(Agent::Claude, "hello");
    let events = feed(
        &mut w,
        &[
            json!({"type":"user","message":{"content":[{"type":"text","text":"hello"}]}}),
            json!({"type":"assistant","message":{"content":[{"type":"text","text":"Partial"}],"stop_reason":null}}),
            json!({"type":"system","subtype":"turn_duration"}),
        ],
    );
    assert_eq!(events.last(), Some(&Event::Done));
}

#[test]
fn a_different_user_turn_never_starts_the_reply() {
    let mut w = Watcher::new(Agent::Claude, "hello");
    assert!(
        feed(
            &mut w,
            &[
                json!({"type":"user","message":{"content":"something else"}}),
                json!({"type":"assistant","message":{"content":[{"type":"text","text":"no"}],"stop_reason":"end_turn"}}),
            ]
        )
        .is_empty()
    );
    assert!(!w.done());
}

#[test]
fn jsonl_stream_handles_partial_lines_across_reads() {
    let mut w = Watcher::new(Agent::Claude, "hi");
    let line1 = r#"{"type":"user","message":{"content":"hi"}}"#;
    let line2 = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Yo."}],"stop_reason":"end_turn"}}"#;
    let all = format!("{line1}\n{line2}\n");
    let (a, b) = all.split_at(30);
    let mut events = w.feed_bytes(a.as_bytes());
    events.extend(w.feed_bytes(b.as_bytes()));
    assert_eq!(
        events,
        vec![Event::Started, Event::Text("Yo.".into()), Event::Done]
    );
}

#[test]
fn interrupted_turns_end_the_watch() {
    let mut w = Watcher::new(Agent::Codex, "go");
    let events = feed(
        &mut w,
        &[
            json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"go"}]}}),
            json!({"type":"event_msg","payload":{"type":"turn_aborted","reason":"interrupted"}}),
        ],
    );
    assert_eq!(events.last(), Some(&Event::Done));
    let mut w = Watcher::new(Agent::Claude, "go");
    let events = feed(
        &mut w,
        &[
            json!({"type":"user","message":{"content":"go"}}),
            json!({"type":"user","message":{"content":[{"type":"text","text":"[Request interrupted by user]"}]}}),
        ],
    );
    assert_eq!(events.last(), Some(&Event::Done));
}

#[test]
fn a_thinking_only_end_turn_record_does_not_end_the_claude_reply() {
    // Claude Code writes thinking as its own record, already marked end_turn,
    // before the record with the reply text.
    let mut w = Watcher::new(Agent::Claude, "what is two plus two");
    let events = feed(
        &mut w,
        &[
            json!({"type":"user","message":{"content":"what is two plus two"}}),
            json!({"type":"assistant","message":{"content":[{"type":"thinking","thinking":""}],"stop_reason":"end_turn"}}),
            json!({"type":"assistant","message":{"content":[{"type":"text","text":"Four."}],"stop_reason":"end_turn"}}),
            json!({"type":"system","subtype":"turn_duration"}),
        ],
    );
    assert_eq!(
        events,
        vec![Event::Started, Event::Text("Four.".into()), Event::Done]
    );
}
