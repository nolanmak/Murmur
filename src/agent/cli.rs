//! `murmur speak`, `murmur mcp` and `murmur agent …`.
use super::{mcp, speech};
use std::io::Read;

pub const USAGE: &str = "speak   [--speed 0.75..2] [--text TEXT] (or pipe text) reads text aloud\nmcp     Serve speak, current_time and session_history to Codex/Claude over stdio\nagent   run codex|claude [--resume ID]   native session owned by Murmur; Ctrl+] detaches\n        list | use ID | send [--id ID] [--speak] TEXT | stop\n        key on|off|status   hold Option to talk to the active session (Control stays dictation)\n        voice status|system|deepgram [MODEL]|speed N";
const AGENT_USAGE: &str = "Usage: murmur agent run codex|claude [--resume ID] | list | use ID | send [--id ID] [--speak] TEXT | stop | key on|off|status | voice …";

pub fn main(command: &str, args: &[String]) -> Result<(), String> {
    match command {
        "speak" => speak(args),
        "mcp" => mcp::serve(
            std::io::stdin().lock(),
            std::io::stdout(),
            std::sync::Arc::new(mcp::System::default()),
        ),
        "agent" => agent(args),
        #[cfg(unix)]
        "agentd" => super::daemon::start(super::daemon::Config::system()?).map(|d| d.wait()),
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
        #[cfg(unix)]
        Some("run") => run(&args[1..]),
        #[cfg(unix)]
        Some("list") => {
            let sessions = super::client::list(&super::daemon::Config::dir()?)?;
            if sessions.is_empty() {
                println!("No Murmur agent sessions. Start one with: murmur agent run codex");
            }
            for s in sessions {
                println!(
                    "{}{} {} {} {}",
                    if s.active { "* " } else { "  " },
                    s.agent,
                    s.id,
                    if s.live { "live" } else { "exited" },
                    s.cwd
                );
            }
            Ok(())
        }
        #[cfg(unix)]
        Some("send") => send(&args[1..]),
        #[cfg(unix)]
        Some("key") => {
            println!(
                "{}",
                super::voice::key_command(&args[1..], &speech::Settings::path()?)?
            );
            Ok(())
        }
        #[cfg(unix)]
        Some("use") => {
            let id = args.get(1).ok_or(AGENT_USAGE)?;
            super::client::activate(&super::daemon::Config::dir()?, id)?;
            println!("Dictation and untargeted sends now go to {id}.");
            Ok(())
        }
        #[cfg(unix)]
        Some("stop") => {
            super::client::shutdown(&super::daemon::Config::dir()?)?;
            println!("Murmur agent sessions stopped.");
            Ok(())
        }
        _ => Err(AGENT_USAGE.into()),
    }
}

#[cfg(unix)]
fn send(args: &[String]) -> Result<(), String> {
    let mut id = None;
    let mut speak = false;
    let mut words = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--id" => {
                id = Some(args.get(i + 1).ok_or(AGENT_USAGE)?.clone());
                i += 1;
            }
            "--speak" => speak = true,
            word => words.push(word.to_string()),
        }
        i += 1;
    }
    let text = words.join(" ");
    let reply = super::client::send(
        &super::daemon::Config::dir()?,
        id.as_deref(),
        &text,
        |part| {
            println!("{part}");
        },
    )?;
    if speak && !reply.trim().is_empty() {
        let text: String = reply.chars().take(speech::MAX_TEXT).collect();
        speech::Voicebox::system().speak(&text, None)?;
    }
    Ok(())
}

/// Restores the terminal mode when the attachment ends.
#[cfg(unix)]
struct Raw(Option<rustix::termios::Termios>);
#[cfg(unix)]
impl Raw {
    fn enter() -> Self {
        let stdin = std::io::stdin();
        let Ok(original) = rustix::termios::tcgetattr(&stdin) else {
            return Raw(None);
        };
        let mut raw = original.clone();
        raw.make_raw();
        if rustix::termios::tcsetattr(&stdin, rustix::termios::OptionalActions::Now, &raw).is_err()
        {
            return Raw(None);
        }
        Raw(Some(original))
    }
}
#[cfg(unix)]
impl Drop for Raw {
    fn drop(&mut self) {
        if let Some(original) = &self.0 {
            let _ = rustix::termios::tcsetattr(
                std::io::stdin(),
                rustix::termios::OptionalActions::Now,
                original,
            );
        }
    }
}

#[cfg(unix)]
fn terminal_size() -> (u16, u16) {
    rustix::termios::tcgetwinsize(std::io::stdout())
        .ok()
        .filter(|w| w.ws_col > 0 && w.ws_row > 0)
        .map(|w| (w.ws_col, w.ws_row))
        .unwrap_or((100, 30))
}

#[cfg(unix)]
fn run(args: &[String]) -> Result<(), String> {
    use super::{Agent, client};
    use std::io::Write;
    let agent = match args.first().map(String::as_str) {
        Some("codex") => Agent::Codex,
        Some("claude") => Agent::Claude,
        _ => return Err(AGENT_USAGE.into()),
    };
    let resume = match args.get(1).map(String::as_str) {
        Some("--resume") => Some(args.get(2).ok_or(AGENT_USAGE)?.as_str()),
        None => None,
        _ => return Err(AGENT_USAGE.into()),
    };
    let dir = super::daemon::Config::dir()?;
    client::ensure_daemon(&dir)?;
    let cwd = std::env::current_dir().map_err(|_| "Cannot read the current directory")?;
    let (cols, rows) = terminal_size();
    let id = client::open(&dir, agent, resume, &cwd, cols, rows)?;
    let tty = {
        use std::io::IsTerminal;
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
    };
    let mut attachment = client::attach(&dir, &id, cols, rows, tty)?;
    println!(
        "Connected to native {}: {id}. Ctrl+] detaches; the session keeps running.\r",
        agent.name()
    );
    let raw = Raw::enter();
    let (input, keys) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 4096];
        while let Ok(n) = stdin.read(&mut buf) {
            if n == 0 || input.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let mut stdout = std::io::stdout();
    let mut size = (cols, rows);
    let message = loop {
        match attachment.read(std::time::Duration::from_millis(50)) {
            Ok(Some(client::Frame::Output(bytes))) => {
                let _ = stdout.write_all(&bytes).and_then(|_| stdout.flush());
            }
            Ok(Some(client::Frame::Exit(_))) => break "Native agent exited.",
            Ok(None) => {}
            Err(_) => break "Murmur's agent daemon closed the session view.",
        }
        while let Ok(bytes) = keys.try_recv() {
            if let Some(at) = bytes.iter().position(|b| *b == 0x1d) {
                if at > 0 {
                    let _ = attachment.input(&bytes[..at]);
                }
                drop(raw);
                println!("\x1b[0m\nDetached. The native session is still running in Murmur.");
                return Ok(());
            }
            if attachment.input(&bytes).is_err() {
                break;
            }
        }
        let now = terminal_size();
        if now != size {
            size = now;
            let _ = attachment.resize(now.0, now.1);
        }
    };
    drop(raw);
    println!("\x1b[0m\n{message}");
    Ok(())
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
