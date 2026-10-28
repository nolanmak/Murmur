#!/usr/bin/env python3
"""Peak Silero VAD probability per clip, straight from fotw-speechd (#101).

Local Parakeet runs the recognizer only when some 256 ms step reaches 0.9
(src/local_parakeet.rs, SPEECH_PEAK). This prints each clip's step
probabilities as JSON, so the gate's effect on real speech can be checked
without the recognizer in the way.

    python3 scripts/speechd-vad-peaks.py HELPER MODELS_ROOT BASE_DIR CLIP...

CLIP paths are relative to BASE_DIR, 16 kHz mono PCM16 WAV. Speaks protocol
version 1 (FlyOnTheWall docs/SPEECHD_PROTOCOL.md) over the helper's stdio;
opens no socket and downloads nothing.
"""
import json
import os
import struct
import subprocess
import sys
import wave


def main():
    helper, models, base = sys.argv[1:4]
    child = subprocess.Popen([helper, "serve", "--models-root", models],
                             stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)

    def frame(kind, body):
        child.stdin.write(struct.pack(">I", len(body) + 1) + kind + body)
        child.stdin.flush()

    def send(message):
        frame(b"\x01", json.dumps(message).encode())

    def receive():
        length = struct.unpack(">I", child.stdout.read(4))[0]
        child.stdout.read(1)
        return json.loads(child.stdout.read(length - 1))

    def call(message):
        send(message)
        while True:
            reply = receive()
            if reply.get("id") == message["id"] and reply["type"] != "progress":
                if reply["type"] == "error":
                    sys.exit(f"{message['type']}: {reply}")
                return reply

    call({"type": "hello", "id": 0, "protocol": 1, "client": "speechd-vad-peaks"})
    call({"type": "load", "id": 1, "engine": "silero-vad"})
    result = {}
    for stream, clip in enumerate(sys.argv[4:], start=1):
        with wave.open(os.path.join(base, clip)) as w:
            pcm = w.readframes(w.getnframes())
        call({"type": "open_stream", "id": 1000 + stream, "stream": stream, "engine": "silero-vad",
              "sample_rate": 16000, "format": "s16le", "channels": 1,
              "options": {"probabilities": True}})
        for i in range(0, len(pcm), 8192):
            frame(b"\x02", struct.pack(">I", stream) + pcm[i:i + 8192])
        send({"type": "finish", "stream": stream})
        steps = []
        while True:
            reply = receive()
            if reply.get("stream") != stream:
                continue
            if reply["type"] == "vad" and reply["event"] == "probability":
                steps.append(round(reply["probability"], 3))
            if reply["type"] in ("closed", "error"):
                break
        result[clip] = steps
    child.stdin.close()
    child.wait(timeout=10)
    print(json.dumps(result))


if __name__ == "__main__":
    main()
