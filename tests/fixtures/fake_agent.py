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
print(f"fake {kind} ready", flush=True)
for line in sys.stdin:
    text = line.replace("\x1b[200~", "").replace("\x1b[201~", "").strip()
    if not text:
        continue
    if text == "exit":
        sys.exit(0)
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
