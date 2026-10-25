//! Optional source-preserving text cleanup. Provider output is an untrusted edit plan.
use serde::{Deserialize, Serialize};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

pub const MAX_TEXT_BYTES: usize = 32 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub version: u8,
    pub remove: Vec<usize>,
    pub breaks: Vec<LineBreak>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LineBreak {
    pub before: usize,
    pub kind: BreakKind,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BreakKind {
    Paragraph,
    Bullet,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Privacy {
    Configured,
    OfflineAll,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    Raw,
    Cleaned,
    Fallback,
    Offline,
    Snippet,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Outcome {
    pub text: String,
    pub disposition: Disposition,
}
impl Outcome {
    /// Preserve the remote sender's existing single-line/printable policy.
    pub fn for_destination(mut self, raw: &str, remote: bool) -> Self {
        if remote
            && self.disposition == Disposition::Cleaned
            && !crate::remote::valid_text(&self.text)
        {
            self.text = raw.to_owned();
            self.disposition = Disposition::Fallback;
        }
        self
    }
}
pub struct Request<'a> {
    pub text: &'a str,
    pub language: &'a str,
}
pub trait Adapter: Send + Sync {
    fn propose<'a>(
        &'a self,
        request: Request<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Plan, String>> + Send + 'a>>;
}
/// Validate every deletion and render exclusively from original source tokens.
pub fn apply_plan(raw: &str, language: &str, plan: &Plan) -> Option<String> {
    let tokens: Vec<&str> = raw.split_whitespace().collect();
    let ordered = |indexes: &[usize]| {
        indexes.windows(2).all(|p| p[0] < p[1]) && indexes.last().is_none_or(|&i| i < tokens.len())
    };
    let breaks: Vec<usize> = plan.breaks.iter().map(|b| b.before).collect();
    if plan.version != 1
        || raw.len() > MAX_TEXT_BYTES
        || tokens.len() > 4096
        || !ordered(&plan.remove)
        || !ordered(&breaks)
    {
        return None;
    }
    if plan.remove.is_empty() && plan.breaks.is_empty() {
        return Some(raw.to_owned());
    }
    let word = |s: &str| {
        s.trim_matches([',', '.', ';', ':', '!', '?'])
            .to_lowercase()
    };
    // Literal/code quotations are deliberately outside this automatic editor.
    if tokens.iter().any(|t| {
        t.contains(['"', '“', '”', '«', '»'])
            || t.starts_with('\'')
            || t.ends_with('\'')
            || matches!(
                word(t).as_str(),
                "verbatim" | "exactly" | "quote" | "quoted" | "literally"
            )
    }) {
        return None;
    }
    if !plan.remove.is_empty() && language.split('-').next() != Some("en") {
        return None;
    }
    let mut removed = vec![false; tokens.len()];
    for &i in &plan.remove {
        removed[i] = true;
    }
    let filler = |s: &str| {
        matches!(
            s.trim_matches([',', '.', ';', ':', '!', '?']),
            "um" | "Um" | "uh" | "Uh" | "erm" | "Erm"
        )
    };
    let mut i = 0;
    while i < tokens.len() {
        if !removed[i] {
            i += 1;
            continue;
        }
        if filler(tokens[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < tokens.len() && removed[i] {
            i += 1;
        }
        // Only a single explicitly corrected source token may be replaced by
        // the following retained token. No clause deletion or generated text.
        if i == tokens.len()
            || word(tokens[start]).ends_with("n't")
            || word(tokens[start]).ends_with("n’t")
            || matches!(
                word(tokens[start]).as_str(),
                "not"
                    | "never"
                    | "no"
                    | "nor"
                    | "without"
                    | "cannot"
                    | "can't"
                    | "don't"
                    | "won't"
                    | "isn't"
                    | "wasn't"
            )
        {
            return None;
        }
        let marker: Vec<String> = tokens[start + 1..i].iter().map(|t| word(t)).collect();
        if marker != ["sorry"] && marker != ["correction"] && marker != ["i", "mean"] {
            return None;
        }
    }
    if plan
        .breaks
        .iter()
        .any(|b| removed[b.before] || (b.before == 0 && b.kind == BreakKind::Paragraph))
    {
        return None;
    }
    let mut output = String::new();
    for (i, token) in tokens.iter().enumerate() {
        if removed[i] {
            continue;
        }
        if let Some(b) = plan.breaks.iter().find(|b| b.before == i) {
            if !output.is_empty() {
                output.push_str(if b.kind == BreakKind::Bullet {
                    "\n"
                } else {
                    "\n\n"
                });
            }
            if b.kind == BreakKind::Bullet {
                output.push_str("- ");
            }
        } else if !output.is_empty() {
            output.push(' ');
        }
        output.push_str(token);
    }
    (!output.trim().is_empty()).then_some(output)
}

/// Bound optional network work and preserve the winning transcript on failure.
pub async fn process(
    raw: &str,
    language: &str,
    enabled: bool,
    privacy: Privacy,
    control: Arc<AtomicU8>,
    deadline: Duration,
    adapter: &dyn Adapter,
) -> Result<Outcome, String> {
    let outcome = |disposition| Outcome {
        text: raw.to_owned(),
        disposition,
    };
    if control.load(Ordering::Acquire) == 2 {
        return Err("Cancelled".into());
    }
    if !enabled {
        return Ok(outcome(Disposition::Raw));
    }
    if privacy == Privacy::OfflineAll {
        return Ok(outcome(Disposition::Offline));
    }
    if raw.trim().is_empty() {
        return Ok(outcome(Disposition::Raw));
    }
    if raw.len() > MAX_TEXT_BYTES || deadline.is_zero() {
        return Ok(outcome(Disposition::Fallback));
    }
    let request = adapter.propose(Request {
        text: raw,
        language,
    });
    let request = tokio::time::timeout(deadline, request);
    tokio::pin!(request);
    let mut cancel = tokio::time::interval(Duration::from_millis(5));
    let result = loop {
        tokio::select! {
            biased;
            _ = cancel.tick() => if control.load(Ordering::Acquire) == 2 { return Err("Cancelled".into()); },
            result = &mut request => break result,
        }
    };
    if control.load(Ordering::Acquire) == 2 {
        return Err("Cancelled".into());
    }
    let Some(text) = result
        .ok()
        .and_then(Result::ok)
        .and_then(|plan| apply_plan(raw, language, &plan))
    else {
        return Ok(outcome(Disposition::Fallback));
    };
    Ok(Outcome {
        disposition: if text == raw {
            Disposition::Raw
        } else {
            Disposition::Cleaned
        },
        text,
    })
}
