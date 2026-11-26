#!/usr/bin/env python3
"""Synthetic stand-in for the codex/claude CLIs (#113). Writes native-shaped
JSONL under $HOME the way the real CLIs do; never contacts a model."""
import json, os, re, sys, uuid

home, cwd, args = os.environ["HOME"], os.getcwd(), sys.argv[1:]
if "--session-id" in args or "--resume" in args:
    kind = "claude"
    flag = "--session-id" if "--session-id" in args else "--resume"
    sid = args[args.index(flag) + 1]
    path = os.path.join(home, ".claude/projects", re.sub(r"[^a-zA-Z0-9]", "-", cwd), sid + ".jsonl")
else:
    kind = "codex"
    sid = args[args.index("resume") + 1] if "resume" in args else str(uuid.uuid4())
    locks = os.path.join(home, ".codex/thread-writer-locks")
    os.makedirs(locks, exist_ok=True)
    lock = open(os.path.join(locks, sid + ".lock"), "w")  # held open, like Codex
    path = os.path.join(home, ".codex/sessions/2026/11/26", f"rollout-2026-11-26T00-00-00-{sid}.jsonl")
os.makedirs(os.path.dirname(path), exist_ok=True)


def record_live(sid):
    if kind == "claude":
        live = os.path.join(home, ".claude/sessions")
        os.makedirs(live, exist_ok=True)
        with open(os.path.join(live, f"{os.getpid()}.json"), "w") as f:
            json.dump({"pid": os.getpid(), "sessionId": sid, "cwd": cwd}, f)


record_live(sid)
if os.path.basename(cwd) == "queries":
    # Like Codex's TUI: query the terminal and wait for the answers.
    import select, termios, tty
    saved = termios.tcgetattr(0)
    tty.setraw(0)
    os.write(1, b"\x1b]11;?\x1b\\\x1b[6n\x1b[c")
    got, deadline = b"", __import__("time").time() + 5
    while not (b"]11;rgb:" in got and b"R" in got and b"[?" in got) and __import__("time").time() < deadline:
        if select.select([0], [], [], 0.1)[0]:
            got += os.read(0, 256)
    termios.tcsetattr(0, termios.TCSADRAIN, saved)
    if b"]11;rgb:" not in got or b"R" not in got:
        print("fake agent got no terminal answers", flush=True)
        sys.exit(3)
inherited = sorted(k for k in os.environ if k in ("CLAUDECODE", "CLAUDE_PID", "CODEX_THREAD_ID") or k.startswith("CLAUDE_CODE_"))
if os.path.basename(cwd) == "dialog":
    # Like a native approval prompt: raw, no echo, repaints on every key
    # (as Ink does), and Enter approves.
    import tty
    tty.setraw(0)
    dialog = "\x1b[H\x1b[2JAllow tool?\r\n> 1. Yes\r\n  2. No\r\n"
    os.write(1, dialog.encode())
    while True:
        b = os.read(0, 1024)
        if b"\r" in b:
            open(os.path.join(home, "APPROVED"), "w").close()
            os.write(1, b"APPROVED\r\n")
        else:
            os.write(1, dialog.encode())
print(f"fake {kind} ready inherited={','.join(inherited) or 'none'}", flush=True)
for line in sys.stdin:
    text = line.replace("\x1b[200~", "").replace("\x1b[201~", "").strip()
    if not text:
        continue
    if text == "exit":
        sys.exit(0)
    if text == "/clear" and kind == "claude":
        sid = str(uuid.uuid4())
        path = os.path.join(os.path.dirname(path), sid + ".jsonl")
        record_live(sid)
        print("cleared", flush=True)
        continue
    if os.path.basename(cwd) == "silent" and not text.startswith("answer"):
        continue  # still working on it
    with open(path, "a") as f:
        if kind == "claude":
            records = [
                {"type": "user", "message": {"role": "user", "content": text}},
                {"type": "assistant", "message": {"content": [{"type": "text", "text": "You said: " + text}], "stop_reason": "end_turn"}},
            ]
        else:
            records = [
                {"type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": text}]}},
                {"type": "response_item", "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Codex heard: " + text}]}},
                {"type": "event_msg", "payload": {"type": "task_complete"}},
            ]
        for r in records:
            f.write(json.dumps(r) + "\n")
    print("answered", flush=True)
