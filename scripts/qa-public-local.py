#!/usr/bin/env python3
"""macOS opt-in QA using existing, hash-pinned public assets; never downloads.

Runs direct and cloud-disconnect paths against an owned managed Whisper runtime.
Does not open capture devices, resolve credentials, or perform text insertion.
Model/audio paths and a prebuilt example must be supplied explicitly.
"""
from pathlib import Path
import argparse
import hashlib
import http.client
import json
import re
import socket
import subprocess
import sys
import tempfile
import threading
import time
import shutil
import urllib.parse
from contextlib import contextmanager
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--model', type=Path, required=True)
parser.add_argument('--wav', type=Path, required=True)
parser.add_argument('--server', type=Path, required=True)
parser.add_argument('--adapter', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
parser.add_argument('--runtime-version', default='unverified operator-supplied binary')
parser.add_argument('--cancellation', action='store_true', help='Also measure adapter cancellation, occupied inference capacity and next-request recovery')
args = parser.parse_args()
TREE = Path(__file__).resolve().parents[1]
MODEL = args.model.resolve()
WAV = args.wav.resolve()
BINARY = args.server.resolve()
ADAPTER = args.adapter.resolve()
OUTPUT = args.output.resolve()
ADAPTER_SHA256 = hashlib.sha256(ADAPTER.read_bytes()).hexdigest()
SB = ['/usr/bin/sandbox-exec', '-f', str(TREE / 'scripts/local-whisper-offline.sb')]
REFERENCE_NORMALIZED_SHA256 = '880038ec1c8c450de59e56c613f14ac01b455a934e4752321e07f1a7cbcec0ee'
for (path, size, digest) in [(MODEL, 487614201, 'c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d'), (WAV, 352078, '59dfb9a4acb36fe2a2affc14bacbee2920ff435cb13cc314a08c13f66ba7860e')]:
    assert path.stat().st_size == size
    actual = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda : stream.read(1024 * 1024), b''):
            actual.update(chunk)
    assert actual.hexdigest() == digest

def command(argv, timeout=90):
    result = subprocess.run(SB + list(map(str, argv)), cwd=TREE, capture_output=True, text=True, timeout=timeout)
    if result.returncode:
        raise RuntimeError('Owned command failed: ' + result.stderr[-2000:])
    return result

@contextmanager
def owned_state():
    temp = Path(tempfile.mkdtemp(prefix='murmur-approved-runtime-'))
    try:
        yield temp
    finally:
        if (temp / 'state' / 'runtime.json').exists():
            raise RuntimeError(f'Owned runtime may still be active; retained control state at {temp}')
        shutil.rmtree(temp)
with owned_state() as temp:
    state = Path(temp) / 'state'
    helper = [sys.executable, str(TREE / 'scripts/local-whisper.py'), '--app', 'murmur', '--state-dir', str(state)]
    with socket.socket() as probe:
        probe.bind(('127.0.0.1', 0))
        port = probe.getsockname()[1]
    started_runtime = False
    samples = []
    stop_sampler = threading.Event()
    sampler = None
    try:
        imported = json.loads(command(helper + ['import-model', MODEL, '--sha256', 'c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d']).stdout)
        assert imported['sha256'] == 'c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d'
        began = time.monotonic()
        ready = json.loads(command(helper + ['start', '--binary', BINARY, '--port', port, '--timeout', 60]).stdout)
        startup_seconds = time.monotonic() - began
        started_runtime = True
        assert ready['ready'] and ready['murmur_capabilities']['model_sha256'] == imported['sha256']
        assert ready['url'] == f'http://127.0.0.1:{port}'
        gateway_pids = [int(x[1:]) for x in subprocess.check_output(['/usr/sbin/lsof', '-nP', f'-iTCP:{port}', '-sTCP:LISTEN', '-Fp'], text=True).splitlines() if x.startswith('p')]
        assert len(gateway_pids) == 1
        gateway = gateway_pids[0]
        children = [int(x) for x in subprocess.check_output(['/usr/bin/pgrep', '-P', str(gateway)], text=True).split()]
        assert len(children) == 1
        engine = children[0]
        comm = subprocess.check_output(['/bin/ps', '-p', str(engine), '-o', 'comm='], text=True).strip()
        assert Path(comm).resolve() == BINARY
        engine_identity = subprocess.check_output(['/bin/ps', '-p', str(engine), '-o', 'lstart=,comm='], text=True, timeout=10)

        def sample():
            while not stop_sampler.is_set():
                value = subprocess.run(['/bin/ps', '-p', f'{gateway},{engine}', '-o', 'pid=,rss='], capture_output=True, text=True)
                for line in value.stdout.splitlines():
                    (pid, kib) = map(int, line.split())
                    samples.append((time.monotonic(), pid, kib * 1024))
                stop_sampler.wait(0.05)
        sampler = threading.Thread(target=sample)
        sampler.start()
        trials = []
        for number in range(6):
            mode = 'direct' if number < 3 else 'cloud-loss'
            flags = [] if mode == 'direct' else ['--simulate-cloud-loss']
            began = time.monotonic()
            result = command(['/usr/bin/time', '-l', ADAPTER, *flags, ready['url'], WAV, 'en'], timeout=120)
            elapsed = time.monotonic() - began
            normalized = ' '.join(re.findall('[a-z0-9]+', result.stdout.lower()))
            matched = hashlib.sha256(normalized.encode()).hexdigest() == REFERENCE_NORMALIZED_SHA256
            assert matched, 'Public reference mismatch (transcript intentionally omitted)'
            recovery = None
            if mode == 'cloud-loss':
                recovery = json.loads(next((line.split('=', 1)[1] for line in result.stderr.splitlines() if line.startswith('fixture_recovery='))))
                assert recovery['input_samples'] == 176000 and recovery['cloud_samples_before_disconnect'] == 88000
                assert recovery['additional_samples_after_disconnect'] == 88000
                assert recovery['partial_cloud_text_discarded'] and recovery['result_provider'] == 'Local Whisper'
                assert recovery['real_provider_calls'] == 0 and (not recovery['insertion_performed'])
            peak = re.search('(\\d+)\\s+maximum resident set size', result.stderr)
            assert peak
            trials.append({'mode': mode, 'trial': number % 3 + 1, 'adapter_wall_seconds': elapsed, 'adapter_max_rss_bytes': int(peak.group(1)), 'reference_matches': matched, 'reference_words': 22, 'recovery': recovery})
            print(mode, 'trial', number % 3 + 1, 'seconds', round(elapsed, 3), 'reference_matches', matched, flush=True)
        cancellation_trials = []
        if args.cancellation:
            address = urllib.parse.urlsplit(ready['url'])
            caps = ready['murmur_capabilities']

            def capacity_status():
                # A bound zero-byte POST never enters inference: free -> 413;
                # occupied -> 429. Health alone does not establish free capacity.
                conn = http.client.HTTPConnection(address.hostname, address.port, timeout=2)
                try:
                    conn.request('POST', '/inference', body=b'', headers={
                        'X-Murmur-Instance': caps['instance_id'],
                        'X-Murmur-Model': caps['model_sha256'],
                    })
                    response = conn.getresponse()
                    response.read(2048)
                    return response.status
                finally:
                    conn.close()

            for number in range(3):
                result = command([ADAPTER, '--cancel-after-ms', '50', ready['url'], WAV, 'en'], timeout=15)
                exited = time.monotonic()
                assert not result.stdout.strip(), 'Cancellation fixture must not return transcript text'
                cancellation = json.loads(next(line.split('=', 1)[1] for line in result.stderr.splitlines() if line.startswith('fixture_cancellation=')))
                assert cancellation['cancelled'] and not cancellation['transcript_returned']
                assert cancellation['input_samples'] == 176000
                assert not cancellation['insertion_performed'] and cancellation['real_provider_calls'] == 0
                probes = []
                while time.monotonic() - exited < 10:
                    status = capacity_status()
                    probes.append({'status': status, 'after_adapter_exit_seconds': time.monotonic() - exited})
                    if status == 413:
                        break
                    assert status == 429, 'Runtime did not preserve its bound ready instance after cancellation'
                    time.sleep(.02)
                assert probes and probes[-1]['status'] == 413, 'Inference capacity did not recover within the fixture deadline'
                assert probes[0]['status'] == 429, 'No occupied inference slot observed after cancellation; this trial cannot establish continued work'
                began = time.monotonic()
                recovered = command([ADAPTER, ready['url'], WAV, 'en'], timeout=120)
                recovery_seconds = time.monotonic() - began
                normalized = ' '.join(re.findall('[a-z0-9]+', recovered.stdout.lower()))
                matched = hashlib.sha256(normalized.encode()).hexdigest() == REFERENCE_NORMALIZED_SHA256
                assert matched, 'Post-cancel reference mismatch (transcript intentionally omitted)'
                cancellation_trials.append({'trial': number + 1, 'adapter': cancellation,
                    'slot_probes': probes, 'next_request_seconds': recovery_seconds,
                    'next_request_reference_matches': matched, 'next_request_reference_words': 22})
                print('cancellation trial', number + 1, 'cancel return seconds', round(cancellation['cancel_to_return_seconds'], 4),
                      'capacity recovered after adapter exit seconds', round(probes[-1]['after_adapter_exit_seconds'], 3),
                      'next reference matches', matched, flush=True)
        stop_sampler.set()
        sampler.join()
        peaks = {str(pid): max((value for (_, p, value) in samples if p == pid)) for pid in [gateway, engine]}
        report = {
            'source_tracked_changes': bool(subprocess.check_output(['git', 'diff', 'HEAD', '--name-only'], cwd=TREE, text=True).strip()),
            'example_sha256': hashlib.sha256((TREE / 'examples/local_transcribe.rs').read_bytes()).hexdigest(),
            'adapter_binary_sha256': ADAPTER_SHA256,
            'source_commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=TREE, text=True).strip(),
            'hardware_model': subprocess.check_output(['/usr/sbin/sysctl', '-n', 'hw.model'], text=True).strip(),
            'ram_bytes': int(subprocess.check_output(['/usr/sbin/sysctl', '-n', 'hw.memsize'], text=True)),
            'macos': subprocess.check_output(['/usr/bin/sw_vers', '-productVersion'], text=True).strip(),
            'runtime_binary_name': BINARY.name,
            'runtime_version': args.runtime_version,
            'runtime_binary_sha256': hashlib.sha256(BINARY.read_bytes()).hexdigest(),
            'model_sha256': imported['sha256'],
            'wave_sha256': '59dfb9a4acb36fe2a2affc14bacbee2920ff435cb13cc314a08c13f66ba7860e',
            'wave_samples': 176000,
            'audio_seconds': 11,
            'adapter': 'actual local_stt direct and fallback::transcribe_with_profile cloud-loss paths',
            'gateway': 'actual managed helper with instance/model binding',
            'external_network_denied': True,
            'startup_to_ready_seconds': startup_seconds,
            'cache_controlled': False,
            'transcript_retained': False,
            'model_import_preceded_startup': True,
            'runtime_settings': 'helper defaults, including native automatic GPU selection; no thread override',
            'rss_sampling_interval_seconds': 0.05,
            'gateway_sampled_peak_rss_bytes': peaks[str(gateway)],
            'engine_sampled_peak_rss_bytes': peaks[str(engine)],
            'startup_rss_measured': False,
            'gpu_allocation_separately_measured': False,
            'sum_rss_is_not_unique_physical_memory': True,
            'trials': trials,
            'cancellation': {
                'trials': cancellation_trials,
                'capacity_measurement': 'Bound empty POST: 429 while occupied, 413 once free; probes do not invoke inference',
                'capacity_time_origin': 'adapter process exit, after cancellation acknowledgement; includes HTTP probe/poll latency',
                'probe_interval_seconds': .02,
                'engine_stopped_or_restarted': False,
                'gpu_kernels_instrumented': False,
                'immediate_compute_interruption_claimed': False,
                'native_escape_or_insertion_tested': False,
            } if args.cancellation else None,
            'input_pacing': 'accelerated two-part public fixture, not realtime capture',
            'physical_wifi_failure': False,
            'physical_capture_or_insertion': False,
            'paired_provider_accuracy': False,
            'concurrent_apps': False,
        }
        assert hashlib.sha256(ADAPTER.read_bytes()).hexdigest() == ADAPTER_SHA256, 'Adapter binary changed during trials'
        assert subprocess.check_output(['/bin/ps', '-p', str(engine), '-o', 'lstart=,comm='], text=True, timeout=10) == engine_identity, 'Owned engine identity changed during trials'
    finally:
        stop_sampler.set()
        if sampler:
            sampler.join(timeout=5)
        stopped = json.loads(command(helper + ['stop']).stdout)
        assert not (state / 'runtime.json').exists(), 'Owned supervisor still active; do not delete its state'
        if started_runtime:
            assert stopped['stopped']
report['owned_runtime_stopped'] = True
report['temporary_control_state_removed'] = not temp.exists()
OUTPUT.write_text(json.dumps(report, indent=2) + '\n')
print('Owned managed runtime stopped; temporary model copy/control state removed. Original approved assets preserved.', flush=True)
