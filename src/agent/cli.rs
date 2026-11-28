//! `murmur speak`, `murmur mcp` and `murmur agent voice`.
use super::{mcp, speech};
use std::io::Read;

pub const USAGE: &str = "speak   [--speed 0.75..2] [--text TEXT] (or pipe text) reads text aloud\nmcp     Serve speak, current_time and session_history to Codex/Claude over stdio\nagent   voice status|system|deepgram [MODEL]|speed N   the voice agents speak with\n\nHold Control+Option to dictate a message the agent should answer aloud.";

pub fn main(command: &str, args: &[String]) -> Result<(), String> {
    match command {
        "speak" => speak(args),
        "mcp" => mcp::serve(
            std::io::stdin().lock(),
            std::io::stdout(),
            std::sync::Arc::new(mcp::System::default()),
        ),
        "agent" => agent(args),
        _ => Err("Unknown command; use --help".into()),
    }
}

fn agent(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("voice") => {
            println!(
                "{}",
                speech::command(&args[1..], &speech::Settings::path()?)?
            );
            Ok(())
        }
        _ => Err(speech::VOICE_USAGE.into()),
    }
}

/// `--speed N` and `--text …`; otherwise stdin, then remaining arguments.
pub fn speak_args(
    args: &[String],
    stdin: impl FnOnce() -> Option<String>,
) -> Result<(String, Option<f32>), String> {
    let mut rest = Vec::new();
    let mut speed = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--speed" => {
                let value = args
                    .get(i + 1)
                    .ok_or("Use --speed with 0.75, 1, 1.25, 1.5, 1.75 or 2")?;
                speed =
                    Some(speech::speed(value.parse::<f64>().map_err(
                        |_| "Use a speed of 0.75, 1, 1.25, 1.5, 1.75 or 2",
                    )?)?);
                i += 2;
            }
            "--text" => return Ok((args[i + 1..].join(" "), speed)),
            _ => {
                rest.push(args[i].clone());
                i += 1;
            }
        }
    }
    if rest.is_empty() {
        return Ok((stdin().unwrap_or_default(), speed));
    }
    Ok((rest.join(" "), speed))
}

fn speak(args: &[String]) -> Result<(), String> {
    let (text, speed) = speak_args(args, || {
        use std::io::IsTerminal;
        let mut stdin = std::io::stdin();
        if stdin.is_terminal() {
            return None;
        }
        let mut text = String::new();
        stdin
            .by_ref()
            .take(64 * 1024)
            .read_to_string(&mut text)
            .ok()?;
        Some(text)
    })?;
    speech::validate_text(&text)?;
    let voice = speech::Voicebox::system().speak(&text, speed)?;
    println!("Spoken ({voice}).");
    Ok(())
}
