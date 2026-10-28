#!/usr/bin/env python3
"""Real-model VAD acceptance suite (Murmur #99). Not part of the offline tests.

Sends every corpus clip through a running managed gateway exactly as Murmur
does (health preflight, bound instance/model headers, explicit language) and
reports inserted words on non-speech, WER on speech and deletion rate on
speech in music/noise. Run it once against a VAD-off runtime and once against
a VAD-on runtime with the same model, then compare.

    python3 -I scripts/eval-local-vad.py --url http://127.0.0.1:PORT \
        --corpus /path/to/corpus-v2 --out results.json
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import secrets
import time
import urllib.request


def request(url, data=None, headers=None, timeout=200):
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with opener.open(urllib.request.Request(url, data, headers or {}), timeout=timeout) as r:
        return json.load(r)


def transcribe(url, caps, audio):
    boundary = "eval-" + secrets.token_hex(16)
    fields = [("file", audio), ("language", b"en"), ("response_format", b"json"),
              ("translate", b"false"), ("detect_language", b"false")]
    body = b""
    for name, value in fields:
        extra = '; filename="audio.wav"\r\nContent-Type: audio/wav' if name == "file" else ""
        body += (f'--{boundary}\r\nContent-Disposition: form-data; name="{name}"{extra}\r\n\r\n'
                 .encode() + value + b"\r\n")
    body += f"--{boundary}--\r\n".encode()
    headers = {"Content-Type": "multipart/form-data; boundary=" + boundary,
               "X-Murmur-Instance": caps["instance_id"], "X-Murmur-Model": caps["model_sha256"]}
    return request(url + "/inference", body, headers)["text"]


def inserted(text):
    # What Murmur would type: the transcript's whitespace-separated tokens.
    return text.split()


def normalize(text):
    return re.sub(r"[^A-Z0-9' ]+", " ", text.upper().replace("-", " ")).split()


def edits(ref, hyp):
    """Return (substitutions, deletions, insertions) of a minimum alignment."""
    rows = [(j, 0, 0, j) for j in range(len(hyp) + 1)]  # (cost, S, D, I)
    for i in range(1, len(ref) + 1):
        current = [(i, 0, i, 0)]
        for j in range(1, len(hyp) + 1):
            same = ref[i - 1] == hyp[j - 1]
            c, s, d, n = rows[j - 1]
            options = [(c + (not same), s + (not same), d, n)]
            c, s, d, n = rows[j]
            options.append((c + 1, s, d + 1, n))
            c, s, d, n = current[j - 1]
            options.append((c + 1, s, d, n + 1))
            current.append(min(options))
        rows = current
    return rows[-1][1:]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", required=True)
    parser.add_argument("--corpus", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--speech-in-snr", type=float, default=10)
    args = parser.parse_args()
    manifest_path = args.corpus / "manifest.json"
    manifest = json.loads(manifest_path.read_text())
    clips = manifest["files"] if isinstance(manifest, dict) else manifest
    health = request(args.url + "/health", timeout=5)
    caps = health["murmur_capabilities"]
    selected = [c for c in clips
                if (c["category"] in ("music", "noise") and c.get("kind") == "dictation_clip")
                or c["category"] in ("speech", "silence")
                or (c["category"] == "speech_in_music" and c.get("snr_db") == args.speech_in_snr)]
    rows = []
    for clip in selected:
        audio = (args.corpus / clip["path"]).read_bytes()
        started = time.monotonic()
        text = transcribe(args.url, caps, audio)
        rows.append({"path": clip["path"], "category": clip["category"],
                     "duration_s": clip.get("duration_s"), "reference": clip.get("reference", ""),
                     "text": text, "seconds": round(time.monotonic() - started, 3)})
        print(json.dumps(rows[-1]), flush=True)

    def words(category):
        return sum(len(inserted(r["text"])) for r in rows if r["category"] == category)

    def alignment(category):
        totals, ref_words = [0, 0, 0], 0
        for r in rows:
            if r["category"] == category:
                ref = normalize(r["reference"])
                ref_words += len(ref)
                totals = [a + b for a, b in zip(totals, edits(ref, normalize(r["text"])))]
        s, d, i = totals
        return {"clips": sum(r["category"] == category for r in rows), "reference_words": ref_words,
                "substitutions": s, "deletions": d, "insertions": i,
                "wer_pct": round(100 * (s + d + i) / max(ref_words, 1), 2),
                "deletion_pct": round(100 * d / max(ref_words, 1), 2)}

    summary = {
        "url": args.url, "capabilities_vad": caps.get("vad", False),
        "model_sha256": caps["model_sha256"],
        "manifest_sha256": hashlib.sha256(manifest_path.read_bytes()).hexdigest(),
        "non_speech": {cat: {"clips": sum(r["category"] == cat for r in rows),
                             "inserted_words": words(cat)} for cat in ("music", "noise", "silence")},
        "speech": alignment("speech"),
        "speech_in_music": alignment("speech_in_music"),
    }
    args.out.write_text(json.dumps({"summary": summary, "clips": rows}, indent=1))
    print(json.dumps(summary, indent=1))


if __name__ == "__main__":
    main()
