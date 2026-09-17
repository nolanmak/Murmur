"""Native PipeWire → Murmur → fake WebSocket test. No real audio or cloud keys."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time

root = Path(__file__).resolve().parents[1]
subprocess.run(["cargo", "test", "--locked", "--lib", "--no-run"], cwd=root, check=True)
with tempfile.TemporaryDirectory(prefix="murmur-native-") as directory:
    env = dict(os.environ, PIPEWIRE_RUNTIME_DIR=directory, MURMUR_NATIVE_FIXTURE="1")
    env.pop("DEEPGRAM_API_KEY", None)
    config = Path("/usr/share/pipewire/pipewire.conf").read_text().replace("#audiotestsrc   =", "audiotestsrc   =")
    config = config.replace("context.objects = [", '''context.objects = [
    { factory = adapter args = {
      factory.name = audiotestsrc node.name = murmur-fixture
      node.description = "Murmur synthetic fixture" media.class = Audio/Source
      audio.channels = 1 audio.position = [ MONO ]
      adapter.auto-port-config = { mode = dsp monitor = false position = preserve }
    } }
    ''', 1)
    path = Path(directory) / "fixture.conf"
    path.write_text(config)
    daemon = subprocess.Popen(["pipewire", "-c", str(path)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    finished = threading.Event()
    errors = []
    def output(*args):
        return subprocess.check_output(args, env=env, text=True, timeout=3)
    def link_inputs():
        configured = set()
        try:
            while not finished.wait(.05):
                if not (Path(directory) / "pipewire-0").exists():
                    continue
                raw = output("pw-dump")
                nodes = {}
                while raw.strip():
                    batch, end = json.JSONDecoder().raw_decode(raw.lstrip())
                    raw = raw.lstrip()[end:]
                    for node in batch:
                        if node.get("info") is None:
                            nodes.pop(node["id"], None)
                        elif "props" in node.get("info", {}):
                            nodes[node["id"]] = node
                nodes = list(nodes.values())
                for node in nodes:
                    if node.get("info", {}).get("props", {}).get("media.class") != "Stream/Input/Audio" or node.get("info", {}).get("props", {}).get("object.serial") in configured:
                        continue
                    output("pw-cli", "set-param", str(node["id"]), "PortConfig", '{ direction = Input mode = dsp format = { mediaType = audio mediaSubtype = raw format = F32P rate = 48000 channels = 1 position = [ MONO ] } }')
                    inputs = output("pw-link", "-i").strip().splitlines()
                    outputs = output("pw-link", "-o").strip().splitlines()
                    if inputs and outputs:
                        output("pw-link", outputs[0], inputs[0])
                        configured.add(node["info"]["props"]["object.serial"])
        except Exception as error:
            errors.append(error)
    linker = threading.Thread(target=link_inputs)
    try:
        deadline = time.monotonic() + 5
        while not (Path(directory) / "pipewire-0").exists():
            if time.monotonic() >= deadline:
                raise RuntimeError("Isolated PipeWire startup timed out")
            time.sleep(.02)
        linker.start()
        result = subprocess.run(["cargo", "test", "--locked", "--lib", "platform::linux::tests::native_pipewire", "--", "--ignored", "--test-threads=1"], cwd=root, env=env, timeout=30)
        if result.returncode:
            raise SystemExit(result.returncode)
        if errors:
            raise errors[0]
    finally:
        finished.set()
        if linker.is_alive():
            linker.join(timeout=5)
        daemon.terminate()
        daemon.wait(timeout=5)
