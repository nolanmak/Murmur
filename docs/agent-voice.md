# Agent voice: Agentflow inside Murmur

Murmur absorbs [Agentflow](https://github.com/nolanmak/Agentflow): talking to
the Codex or Claude Code session you are working in, and hearing it answer.
Agentflow's Node service, browser recorder and its own speech-to-text stack
are not ported. Murmur's capture, hotkey, transcription engines, vocabulary,
Keychain credentials and Offline-all policy do that work.

## How it works

- **Control** is Murmur's dictation key, unchanged: hold, speak, release,
  and the text is typed into the focused field.
- **Control+Option** is the same dictation into the same focused field (your
  agent's terminal), with a short note appended:
  `(Answer aloud with the speak tool.)`. The agent then answers by calling
  the `speak` tool, so you hear the reply in the session you are in, with all
  of its context. Hold the two keys together, or press Option while holding
  Control. Control+Option with another key stays a shortcut.
- **`murmur mcp`** gives agents the tools Agentflow published: `speak`,
  `current_time` and `session_history`.
- **`murmur speak`** reads text aloud from a shell.

```sh
ln -sf /path/to/Murmur.app/Contents/MacOS/murmur ~/.local/bin/murmur
claude mcp add murmur -s user -- ~/.local/bin/murmur mcp
codex mcp add murmur -- ~/.local/bin/murmur mcp
murmur agent voice status        # deepgram [MODEL] | system | speed N
```

| Agentflow | Murmur |
|---|---|
| Browser microphone + turn detection | Hold Control (or Control+Option) |
| `speech.ts` transcribe (Deepgram/OpenAI/ElevenLabs/9router) | Murmur's engines: Parakeet, streaming, Whisper, Deepgram, fallback |
| `speech.ts` synthesize + `afplay` | `agent::speech`: Deepgram Aura or the offline macOS voice |
| Keychain key store | Murmur's Deepgram credential (`config.rs`) |
| `agentflow mcp` (speak, current_time, session_history) | `murmur mcp` |
| `agentflow speak` | `murmur speak` |

## Boundaries

- Control dictation is never redirected; the note is added only when Option
  is part of the hold.
- Speech keys stay where Murmur keeps them; no key is printed, logged or sent
  to an agent. Offline-all forces the macOS voice: no text leaves the Mac.
- Native histories are read only for `session_history`: by UUID, bounded to
  the last 1 MB and 40 messages, never following symlinks.

## History

A first version (#117–#121) ran Codex/Claude in Murmur-owned terminals and
sent dictation to a separate session. It was removed: that session had none
of the user's context and each turn was slow. Voice now goes to the session
the user is in.
