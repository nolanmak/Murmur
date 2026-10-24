#!/usr/bin/env python3
"""Opt-in synthetic whisper.cpp benchmark; requires macOS say and ffmpeg, no downloads.

Run with external networking denied after installing/starting the engine.
This tests the raw engine; application adapter and microphone tests are separate.
"""
import argparse
import json
import math
from pathlib import Path
import re
import subprocess
import tempfile
import time
import urllib.request
from urllib.parse import urlsplit


def words(text):
    return re.findall(r"[a-z0-9]+", text.lower())


def distance(expected, actual):
    row = list(range(len(actual) + 1))
    for i, left in enumerate(expected, 1):
        current = [i]
        for j, right in enumerate(actual, 1):
            current.append(min(current[-1] + 1, row[j] + 1, row[j - 1] + (left != right)))
        row = current
    return row[-1]


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        raise ValueError("Benchmark redirects prohibited")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    url = urlsplit(args.url)
    if url.scheme != "http" or url.hostname not in ("127.0.0.1", "::1") or url.username or url.password or url.path not in ("", "/") or url.query or url.fragment:
        parser.error("Use a literal loopback HTTP engine URL")
    corpus = json.loads(Path(__file__).with_name("local-whisper-corpus.json").read_text())
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    results = []
    with tempfile.TemporaryDirectory(prefix="local-speech-benchmark-") as temp:
        directory = Path(temp)
        for name, reference in corpus.items():
            aiff, wav = directory / "fixture.aiff", directory / "fixture.wav"
            subprocess.run(["/usr/bin/say", "-r", "145", "-o", str(aiff), reference], check=True)
            subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-i", str(aiff), "-ar", "16000", "-ac", "1", "-af", "apad", "-t", "10", "-c:a", "pcm_s16le", str(wav)], check=True)
            boundary = "local-speech-benchmark-fixture"
            body = (f'--{boundary}\r\nContent-Disposition: form-data; name="response_format"\r\n\r\njson\r\n--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="fixture.wav"\r\nContent-Type: audio/wav\r\n\r\n'.encode() + wav.read_bytes() + f'\r\n--{boundary}--\r\n'.encode())
            request = urllib.request.Request(args.url.rstrip("/") + "/inference", data=body, headers={"Content-Type": f"multipart/form-data; boundary={boundary}"})
            start = time.monotonic()
            with opener.open(request, timeout=180) as response:
                transcript = json.load(response)["text"].strip()
            elapsed = time.monotonic() - start
            results.append({"fixture": name, "duration_seconds": 10, "elapsed_seconds": round(elapsed, 3), "reference": reference, "text": transcript, "word_errors": distance(words(reference), words(transcript)), "reference_words": len(words(reference))})
            print(name, round(elapsed, 3), flush=True)
    clean = [row for row in results if row["fixture"].startswith("clean")]
    warm = sorted(row["elapsed_seconds"] for row in clean[1:])
    report = {"description": "Raw engine synthetic TTS fixtures, not human microphone accuracy", "clean_wer": sum(row["word_errors"] for row in clean) / sum(row["reference_words"] for row in clean), "clean_warm_p95_seconds": warm[math.ceil(len(warm) * .95) - 1], "results": results}
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({key: value for key, value in report.items() if key != "results"}))


if __name__ == "__main__":
    main()
