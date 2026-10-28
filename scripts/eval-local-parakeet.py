#!/usr/bin/env python3
"""Real-model acceptance suite for Local Parakeet (#101). Not run in CI.

Builds examples/parakeet_eval, runs it with the network denied
(scripts/local-whisper-offline.sb: loopback only), and scores its output
against the shared corpus manifest (nolanmak/FlyOnTheWall#247 corpus-v2).

    python3 scripts/eval-local-parakeet.py --helper PATH --models DIR \\
        --corpus DIR [--trials 20] [--raw OUT.jsonl] [--short-set DIR]

`--short-set` adds the short and quiet speech set from
scripts/make-short-speech-set.py: one-word utterances and quiet (-12, -20 dB)
speech, scored as recall (every reference word typed) per gain, so the speech
gate's cost to real speech is measured, not assumed.

`--models` must already hold installed parakeet-unified and silero-vad
(`murmur speech install-parakeet`, or `fotw-speechd install ... --from`);
nothing is downloaded here. Prints a JSON report and exits 1 if a gate fails.
"""
import argparse
import json
import math
import os
import re
import subprocess
import sys
import unicodedata

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# Scoring is FlyOnTheWall tools/speech-eval/speech_eval.py
# ("nfc-casefold-technical-numeric-v2"), the normalization the #118 numbers
# used, copied so this script needs no FlyOnTheWall checkout.
NORMALIZATION = "nfc-casefold-technical-numeric-v2"
NUMBER = r"(?:[$€£]?[+-]?|[+-]?[$€£]?)\d+(?:[.,]\d+)*(?:%)?"
TOKEN = re.compile(NUMBER + r"(?!\w|[._:/’'-]+\w)|\w+(?:[._:/’'-]+\w+)*(?:\+\+(?:\d+)?|#)?", re.UNICODE)


def tokenize(text):
    return TOKEN.findall(unicodedata.normalize("NFC", text).casefold())


def errors(reference, hypothesis):
    row = list(range(len(hypothesis) + 1))
    for i, ref in enumerate(reference, 1):
        current = [i]
        for j, hyp in enumerate(hypothesis, 1):
            current.append(min(row[j - 1] + (ref != hyp), row[j] + 1, current[j - 1] + 1))
        row = current
    return row[-1]


def pooled_wer(rows):
    words = sum(len(tokenize(r["reference"])) for r in rows)
    wrong = sum(errors(tokenize(r["reference"]), tokenize(r["text"])) for r in rows)
    return wrong / words if words else None


def percentile(values, p):
    ordered = sorted(values)
    rank = max(1, math.ceil(p / 100 * len(ordered)))
    return ordered[rank - 1]


# src/local_parakeet.rs SPEECH_PEAK.
SPEECH_GATE = 0.9

# Spellings that are the same word for recall.
SAME = {"okay": "ok", "o.k": "ok", "thank": "thanks"}


def words(text):
    return [SAME.get(w, w) for w in tokenize(text)]


def short_report(folder, lines):
    manifest = {f["path"]: f for f in json.load(open(os.path.join(folder, "manifest.json")))["files"]}
    rows = {}
    for line in (l for l in lines if l["kind"] == "clip"):
        meta = manifest[line["path"]]
        row = rows.setdefault((meta["category"], meta["gain_db"]), {
            "kind": meta["category"], "gain_db": meta["gain_db"], "clips": 0, "empty": 0,
            "recalled": 0, "missed": [], "_rows": []})
        row["clips"] += 1
        hypothesis = words(line["text"])
        if not hypothesis:
            row["empty"] += 1
        if all(w in hypothesis for w in words(meta["reference"])):
            row["recalled"] += 1
        else:
            row["missed"].append({"path": line["path"], "reference": meta["reference"], "text": line["text"]})
        row["_rows"].append({"reference": meta["reference"], "text": line["text"]})
    out = []
    for key in sorted(rows, key=lambda k: (k[0], -k[1])):
        row = rows[key]
        row["recall"] = row["recalled"] / row["clips"]
        row["wer"] = pooled_wer(row.pop("_rows"))
        out.append(row)
    return {"rows": out}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--helper", required=True)
    parser.add_argument("--models", required=True)
    parser.add_argument("--corpus", required=True)
    parser.add_argument("--trials", type=int, default=20)
    parser.add_argument("--raw")
    parser.add_argument("--no-sandbox", action="store_true", help="skip the network-deny sandbox")
    parser.add_argument("--short-set", help="directory from scripts/make-short-speech-set.py")
    args = parser.parse_args()

    subprocess.run(["cargo", "build", "--release", "--locked", "--example", "parakeet_eval"],
                   cwd=ROOT, check=True)
    target = os.environ.get("CARGO_TARGET_DIR", os.path.join(ROOT, "target"))
    def evaluate(corpus, trials, categories=None):
        command = [os.path.join(target, "release/examples/parakeet_eval"), "--helper", args.helper,
                   "--models", args.models, "--corpus", corpus, "--trials", str(trials)]
        if categories:
            command += ["--categories", ",".join(categories)]
        if not args.no_sandbox:
            command = ["sandbox-exec", "-f", os.path.join(ROOT, "scripts/local-whisper-offline.sb")] + command
        run = subprocess.run(command, capture_output=True, text=True)
        if run.returncode != 0:
            sys.stderr.write(run.stderr)
            sys.exit(f"parakeet_eval failed ({run.returncode})")
        return [json.loads(line) for line in run.stdout.splitlines() if line.startswith("{")]

    lines = evaluate(args.corpus, args.trials)
    short_lines = []
    peaks = {}
    if args.short_set:
        short_lines = evaluate(args.short_set, 0, ["short/g00", "short/g12", "short/g20"])
        lines += [dict(line, short_set=True) for line in short_lines if line["kind"] == "clip"]
        # The gate's own decision per clip, without the recognizer in the way.
        probe = [sys.executable, "-I", os.path.join(ROOT, "scripts/speechd-vad-peaks.py"),
                 args.helper, args.models, args.short_set] + [l["path"] for l in short_lines if l["kind"] == "clip"]
        if not args.no_sandbox:
            probe = ["sandbox-exec", "-f", os.path.join(ROOT, "scripts/local-whisper-offline.sb")] + probe
        peaks = {path: max(steps, default=0.0)
                 for path, steps in json.loads(subprocess.run(probe, capture_output=True, text=True,
                                                              check=True).stdout).items()}
    if args.raw:
        with open(args.raw, "w") as out:
            out.writelines(json.dumps(line) + "\n" for line in lines)

    manifest = {f["path"]: f for f in json.load(open(os.path.join(args.corpus, "manifest.json")))["files"]}
    clips = [dict(line, **{"reference": manifest[line["path"]].get("reference", ""),
                           "meta": manifest[line["path"]]})
             for line in lines if line["kind"] == "clip" and not line.get("short_set")]

    def subset(prefix, snr=None):
        return [c for c in clips if c["path"].startswith(prefix)
                and (snr is None or c["meta"].get("snr_db") == snr)]

    def inserted(rows):
        return {"clips": len(rows), "words": sum(len(tokenize(r["text"])) for r in rows),
                "clips_with_words": sorted(r["path"] for r in rows if tokenize(r["text"]))}

    music = subset("music/clips/")
    latency = [line["ms"] for line in lines if line["kind"] == "latency"]
    report = {
        "normalization": NORMALIZATION,
        "load": next((line for line in lines if line["kind"] == "load"), None),
        "wer_clean": pooled_wer(subset("speech/")),
        "wer_noise": {snr: pooled_wer(subset("speech_in_noise/", snr)) for snr in (0, 5, 10)},
        "wer_music": {snr: pooled_wer(subset("speech_in_music/", snr)) for snr in (0, 5, 10)},
        "non_speech": {
            "silence": inserted(subset("silence/")),
            "noise": inserted(subset("noise/clips/")),
            "music_instrumental": inserted([c for c in music if "vocal" not in c["meta"].get("tags", [])]),
            "music_vocal": inserted([c for c in music if "vocal" in c["meta"].get("tags", [])]),
        },
        "thank_you": {c["path"]: c["text"] for c in subset("thank_you/")},
        "latency_ms": {"trials": len(latency),
                       "p50": percentile(latency, 50) if latency else None,
                       "p95": percentile(latency, 95) if latency else None,
                       "max": max(latency) if latency else None,
                       "loadavg": sorted({line["loadavg"] for line in lines if line["kind"] == "latency"})},
        "offline": not args.no_sandbox,
    }
    if args.short_set:
        report["short_and_quiet"] = short_report(args.short_set, short_lines)
        report["short_and_quiet"]["vad_peak_min"] = min(peaks.values())
        report["short_and_quiet"]["below_speech_gate"] = sorted(p for p, v in peaks.items() if v < SPEECH_GATE)
    ns = report["non_speech"]
    short = report.get("short_and_quiet", {})
    gates = {
        "latency_p95_le_800ms": bool(latency) and len(latency) >= 20 and report["latency_ms"]["p95"] <= 800,
        "wer_clean_le_2.5pct": report["wer_clean"] is not None and report["wer_clean"] <= 0.025,
        "wer_noise_10db_le_5pct": report["wer_noise"][10] is not None and report["wer_noise"][10] <= 0.05,
        "silence_and_noise_insert_0_words": ns["silence"]["words"] == 0 and ns["noise"]["words"] == 0,
        "instrumental_music_inserts_0_words": ns["music_instrumental"]["words"] == 0,
        # Sung vocals are tracked in Murmur #104; reported, not gated here.
    }
    if short:
        # The gate must pass every short or quiet real utterance. ("empty" in
        # the rows also counts the recognizer's own misses; it is reported.)
        gates["short_and_quiet_speech_passes_the_gate"] = not short["below_speech_gate"]
    report["gates"] = gates
    print(json.dumps(report, indent=2, default=str))
    sys.exit(0 if all(gates.values()) else 1)


if __name__ == "__main__":
    main()
