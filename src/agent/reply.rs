//! Observes one native reply in the agent's own JSONL (#113). The reply
//! starts only after the submitted user turn is recorded, so earlier turns
//! and other writers' text are never read aloud.
use super::{Agent, history};
use serde_json::Value;

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    Started,
    Text(String),
    Done,
}

pub struct Watcher {
    agent: Agent,
    text: String,
    started: bool,
    done: bool,
    rest: Vec<u8>,
}

impl Watcher {
    pub fn new(agent: Agent, text: &str) -> Self {
        Self {
            agent,
            text: text.trim().to_string(),
            started: false,
            done: false,
            rest: Vec::new(),
        }
    }

    pub fn done(&self) -> bool {
        self.done
    }

    pub fn started(&self) -> bool {
        self.started
    }

    fn is_submitted_turn(&self, record: &Value) -> bool {
        history::message(record, self.agent)
            .is_some_and(|m| m.role == "user" && m.text.trim() == self.text)
    }

    pub fn feed(&mut self, record: &Value) -> Vec<Event> {
        let mut events = Vec::new();
        if self.done {
            return events;
        }
        if !self.started {
            if self.is_submitted_turn(record) {
                self.started = true;
                events.push(Event::Started);
            }
            return events;
        }
        if let Some(message) = history::message(record, self.agent)
            && message.role == "assistant"
        {
            events.push(Event::Text(message.text));
        }
        let finished = match self.agent {
            Agent::Codex => {
                record.get("type").and_then(Value::as_str) == Some("event_msg")
                    && record.pointer("/payload/type").and_then(Value::as_str)
                        == Some("task_complete")
            }
            Agent::Claude => {
                let kind = record.get("type").and_then(Value::as_str);
                (kind == Some("assistant")
                    && matches!(
                        record
                            .pointer("/message/stop_reason")
                            .and_then(Value::as_str),
                        Some("end_turn" | "stop_sequence")
                    ))
                    || (kind == Some("system")
                        && record.get("subtype").and_then(Value::as_str) == Some("turn_duration"))
            }
        };
        if finished {
            self.done = true;
            events.push(Event::Done);
        }
        events
    }

    /// Feeds raw appended bytes; a trailing partial line waits for more.
    pub fn feed_bytes(&mut self, bytes: &[u8]) -> Vec<Event> {
        self.rest.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(end) = self.rest.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.rest.drain(..=end).collect();
            if let Ok(record) = serde_json::from_slice::<Value>(&line) {
                events.extend(self.feed(&record));
            }
        }
        events
    }
}
