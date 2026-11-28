# Agent voice: Agentflow inside Murmur

Murmur absorbs [Agentflow](https://github.com/nolanmak/Agentflow): speaking to
native Codex and Claude Code sessions and hearing their replies. Agentflow's
Node service, browser recorder and its own speech-to-text stack are not ported.
Murmur's existing capture, hotkey, local/cloud transcription, vocabulary,
Keychain credentials and Offline-all policy do that work instead.

## What carries over, and from where

| Agentflow | Murmur |
|---|---|
| Browser microphone + RMS turn detection | Hold-Control capture (existing) |
| `speech.ts` transcribe (Deepgram/OpenAI/ElevenLabs/9router) | Existing engines: Parakeet, streaming, Whisper, Deepgram, fallback |
| `speech.ts` synthesize + `afplay` | `agent_speech`: Deepgram Aura or the offline macOS voice |
| Keychain key store | Existing Deepgram credential resolution (`config.rs`) |
| `agentflow mcp` (speak, current_time, session_history) | `murmur mcp` |
| `agentflow speak` | `murmur speak` |
| `terminal.ts` PTY ownership + `agentflow run` | `murmur agentd` + `murmur agent run codex\|claude` |
| Reply observation from native JSONL | `agent::reply` watcher |
| Dashboard voice button | Agent key: hold Right Option |

## Boundaries kept from Agentflow

- Native session identity is preserved. Murmur never copies a transcript into a
  new chat or calls a new chat an attached session; it never mutates native
  history files and reads them only to observe replies (bounded to 1 MB).
- Agent permissions are never bypassed. Murmur launches the native CLI with its
  own arguments only (`--session-id`/`--resume` for Claude, `resume` for Codex).
- Voice text is refused while the native prompt holds unfinished typed text,
  and text containing terminal control characters is refused.
- The control socket lives in a 0700 directory the user owns and is 0600, so
  other users cannot connect. Nothing listens on TCP.
- Voice presses Enter only after a row of the session's screen (a virtual
  terminal Murmur keeps for each session) newly shows the pasted text. A
  dialog (permission, model choice) does not, even when it repaints words like
  "Yes", so voice is refused there instead of answering it. A refused paste
  marks the prompt as holding a draft until the user sends or clears it.
- A session Murmur starts is a new top-level session: identity markers of a
  parent agent session (`CLAUDECODE`, `CLAUDE_CODE_SESSION_ID`, …) are not
  inherited. User configuration variables are.
- Speech keys stay where Murmur already keeps them; no key is printed,
  logged or sent to the agent.
- Offline-all forces the local macOS voice; no text leaves the Mac for speech.
- Nothing starts recording automatically, and starting the daemon never
  starts a microphone.

## Delivery slices

Each slice is its own issue and PR, built test-first (red, green, refactor).
CI never uses a microphone, real provider keys, real agent CLIs or personal
sessions: tests use local fake HTTP servers, fake player commands, fake agent
scripts and synthetic JSONL.

### 1. Spoken replies and the MCP tools

- `murmur speak [--speed 0.75..2] [--text TEXT]` (or stdin) plays text.
- `murmur mcp` serves `speak`, `current_time` and `session_history` over stdio
  with the same tool schemas Agentflow published.
- Settings: `~/Library/Application Support/Murmur/agent-voice.json`
  (`voice`: `deepgram` or `system`, Deepgram `model`, default `speed`).
  `murmur agent voice deepgram [MODEL] | system | speed N | status`.

Acceptance:
- Text outside 1–12000 characters, and speeds outside the allowed set, are
  refused before any network or playback.
- Deepgram requests go to `/v1/speak?model=…&encoding=mp3` with the token
  header, redirects disabled and a deadline; non-audio, empty and >24 MB
  responses fail without playback. Text is chunked at sentence boundaries
  under Deepgram's 2000-character limit and played in order.
- With no Deepgram key, or with Offline-all on, the macOS voice is used and no
  request is made.
- Playback temp files are 0600 and removed after playback or failure.
- MCP `initialize`, `tools/list`, `ping` and each tool behave as Agentflow's;
  tool failures return `isError` results, malformed lines are ignored,
  notifications get no reply.
- `session_history` accepts only a UUID, finds the native file without
  following other users' or symlinked files, and returns at most the last 40
  user/assistant messages with original timestamps.

### 2. Murmur-owned agent sessions

- `murmur agentd` owns native `codex`/`claude` PTYs and serves a Unix socket.
  `murmur agent run codex|claude [--resume ID]` (`murmur run` stays the menu-bar app) starts it on demand and attaches the
  current terminal; Ctrl+] detaches and the session keeps running.
- `murmur agent list` shows live sessions; `murmur agent send [--id ID]
  [--speak] TEXT` delivers text as one native user turn and prints (and
  optionally speaks) the reply.

Acceptance:
- Attach replays recent output, forwards input and resize, and a detach or a
  client crash never kills the native process.
- A Codex session's real id replaces its provisional id once Codex creates its
  thread (from the writer lock it holds), and both ids resolve.
- Voice input is a bracketed paste followed by Enter; it is refused with a
  clear message while typed text is pending and for control characters.
- The reply watcher starts only after the submitted user turn appears in the
  native JSONL, streams assistant text, and ends at Codex `task_complete` or
  Claude `end_turn`/`turn_duration`. Exit or a 15-minute deadline fails
  explicitly.
- A newer voice turn supersedes an older one's reply watch (the native agent
  queues both turns). Esc interrupts (`turn_aborted`, "[Request interrupted
  by user]") end a watch, and Murmur follows `/clear` and `/new` to the
  session's new history file.

### 3. Agent key

- **Control is always Murmur's dictation key** and types into the focused
  field exactly as before. Agent voice never redirects it.
- **Holding Right Option** (350 ms, like Control) records for the agent: the
  transcript goes to the active Murmur agent session and the reply is spoken.
  Quick taps and Option+key shortcuts (accents, symbols) pass through.
- The key is on by default. Turn it off or on with `murmur agent key off|on`
  or the menu item "Agent key (hold Right Option)". The running app picks a
  CLI change up within a second.
- The active session is the one most recently started, attached or sent to;
  choose another with `murmur agent use ID` (`murmur agent list` marks it).
- Agent recordings need only the microphone: nothing is inserted, so no
  focused text field or Accessibility grant is required.

Acceptance:
- With the agent key on or off, Control dictation is unchanged.
- With no live session, nothing is pasted or sent; the status says how to
  start one (`murmur agent run codex`), and the text stays available through
  Copy Last Transcript.
- Starting a new recording stops any reply still being spoken; a turn that a
  newer one supersedes ends silently.
- Status shows sending, waiting and replied/failed stages without transcript
  or reply text.

## Using it

```sh
ln -sf /path/to/Murmur.app/Contents/MacOS/murmur ~/.local/bin/murmur
cd ~/project && murmur agent run claude   # answer any startup prompts, Ctrl+] to detach
# Hold Right Option and speak (Control stays normal dictation).
murmur agent send --speak "summarise the failing tests"   # same thing from a shell
claude mcp add murmur -- murmur mcp        # replaces the agentflow MCP server
codex mcp add murmur -- murmur mcp
```

Acceptance:
- With agent voice off, delivery is unchanged.
- With it on and no live session, nothing is pasted or sent, the status says
  how to start one (`murmur agent run codex`) and the text stays available through
  Copy Last Transcript.
- Starting a new dictation stops any reply that is still being spoken.
- Status shows sending, waiting and replied/failed stages without transcript
  or reply text.

## Follow-ups (scoped, not in these slices)

- Hands-free loop: VAD end-of-turn → send → speak → listen again, with
  explicit barge-in.
- Dashboard/web view of sessions, and external (non-Murmur) Codex sessions
  through `codex queue`.
- Speaking each sentence as it streams instead of after the turn completes.
