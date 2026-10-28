#!/usr/bin/env python3
"""Build the short and quiet speech set for Local Parakeet's speech gate (#101).

Parakeet runs only when Silero VAD reaches 0.9 somewhere in the utterance.
This set checks that the gate keeps real speech that is short (one word) or
quiet (far from the mic):

- `say` (macOS TTS) speaking "Yes.", "OK.", "No.", "Sure.", "Thanks." in six
  voices, padded with 0.5 s of silence each side. Generated locally; not for
  redistribution, and nothing here goes into Git.
- LibriSpeech test-clean utterances of one or two words, decoded from a local
  LibriSpeech directory (`--librispeech .../LibriSpeech/test-clean`).
- The corpus-v2 `thank_you/` clips and the 36 `speech/` clips (`--corpus`).

Each clip is written at 0, -12 and -20 dB gain into `<out>/short/gNN/`
(`short/g00`, `short/g12`, `short/g20`), 16 kHz mono PCM16, with a
`manifest.json` that eval-local-parakeet.py reads for references.

    python3 scripts/make-short-speech-set.py --out DIR --corpus CORPUS_V2 \\
        --librispeech LIBRISPEECH/test-clean
"""
import argparse
import array
import json
import os
import subprocess
import tempfile
import wave

WORDS = ["Yes.", "OK.", "No.", "Sure.", "Thanks."]
VOICES = ["Samantha", "Daniel", "Karen", "Moira", "Rishi", "Tessa"]
GAINS_DB = [0, -12, -20]
RATE = 16000


def read_pcm(path):
    with wave.open(path) as w:
        assert (w.getframerate(), w.getnchannels(), w.getsampwidth()) == (RATE, 1, 2), path
        return array.array("h", w.readframes(w.getnframes()))


def write_pcm(path, samples):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with wave.open(path, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes(samples.tobytes())


def decode(source, scratch):
    """Any audio file to 16 kHz mono PCM16 samples."""
    out = os.path.join(scratch, "decoded.wav")
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-i", source, "-ac", "1", "-ar", str(RATE),
                    "-sample_fmt", "s16", out], check=True)
    return read_pcm(out)


def pad(samples, seconds=0.5):
    silence = array.array("h", [0] * int(RATE * seconds))
    return silence + samples + silence


def gain(samples, db):
    scale = 10 ** (db / 20)
    return array.array("h", (max(-32768, min(32767, round(s * scale))) for s in samples))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", required=True)
    parser.add_argument("--corpus", required=True, help="corpus-v2 directory")
    parser.add_argument("--librispeech", help="LibriSpeech test-clean directory")
    args = parser.parse_args()

    clips = []  # (name, kind, reference, samples)
    with tempfile.TemporaryDirectory() as scratch:
        for voice in VOICES:
            for word in WORDS:
                aiff = os.path.join(scratch, "say.aiff")
                subprocess.run(["say", "-v", voice, "-o", aiff, word], check=True)
                name = f"tts-{word.strip('.').lower()}-{voice.lower()}"
                clips.append((name, "tts-short-word", word, pad(decode(aiff, scratch))))
        if args.librispeech:
            for root, _, files in sorted(os.walk(args.librispeech)):
                for transcript in (f for f in files if f.endswith(".trans.txt")):
                    for line in open(os.path.join(root, transcript)):
                        utterance, text = line.split(" ", 1)
                        if len(text.split()) <= 2:
                            flac = os.path.join(root, utterance + ".flac")
                            clips.append((f"ls-{utterance}", "librispeech-short", text.strip(),
                                          pad(decode(flac, scratch))))
    manifest = json.load(open(os.path.join(args.corpus, "manifest.json")))["files"]
    for entry in manifest:
        if entry["category"] in ("thank_you", "speech"):
            name = os.path.splitext(os.path.basename(entry["path"]))[0]
            clips.append((name, entry["category"], entry["reference"],
                          read_pcm(os.path.join(args.corpus, entry["path"]))))

    files = []
    for db in GAINS_DB:
        folder = f"short/g{abs(db):02d}"
        for name, kind, reference, samples in clips:
            path = f"{folder}/{name}.wav"
            write_pcm(os.path.join(args.out, path), gain(samples, db))
            files.append({"path": path, "category": kind, "gain_db": db, "reference": reference})
    with open(os.path.join(args.out, "manifest.json"), "w") as out:
        json.dump({"name": "Local Parakeet short and quiet speech", "files": files}, out, indent=1)
    print(f"{len(files)} clips in {args.out}")


if __name__ == "__main__":
    main()
