"""Bounded loopback gateway for an owned, fixed-model whisper.cpp process."""
import email.parser
import email.policy
import http.client
import http.server
import io
import json
import re
import secrets
import socketserver
import threading
import time
import wave

# Language token IDs, in order, from whisper.cpp v1.9.1 src/whisper.cpp g_lang.
# https://github.com/ggml-org/whisper.cpp/blob/v1.9.1/src/whisper.cpp
LANGUAGES = tuple("en zh de es ru ko fr ja pt tr pl ca nl ar sv it id hi fi vi he uk el ms cs ro da hu ta no th ur hr bg lt la mi ml cy sk te fa lv bn sr az sl kn et mk br eu is hy ne mn bs kk sq sw gl mr pa si km sn yo so af oc ka be tg sd gu am yi lo uz fo ht ps tk nn mt sa lb my bo tl mg as tt haw ln ha ba jw su yue".split())
MAX_BODY = 4 * 1024 * 1024
MAX_RESPONSE = 1024 * 1024
MAX_FRAMES = 16000 * 120 + 3200
BODY_SECONDS = 3
# A line that is nothing but Whisper sound annotations: *music*, [BLANK_AUDIO], (applause).
ANNOTATION_LINE = re.compile(r"\s*(?:(?:\*[^*\n]*\*|\[[^\]\n]*\]|\([^)\n]*\))\s*)+")
INFERENCE_SECONDS = 180


class Refused(Exception):
    def __init__(self, status, message):
        self.status, self.message = status, message


class BoundedServer(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
    block_on_close = False

    def __init__(self, address, handler):
        self.slots = threading.BoundedSemaphore(4)
        super().__init__(address, handler)

    def server_bind(self):
        socketserver.TCPServer.server_bind(self)
        self.server_name, self.server_port = self.server_address[:2]

    def process_request(self, request, address):
        if not self.slots.acquire(blocking=False):
            self.shutdown_request(request)
            return
        try:
            super().process_request(request, address)
        except BaseException:
            self.slots.release()
            raise

    def process_request_thread(self, request, address):
        try:
            super().process_request_thread(request, address)
        finally:
            self.slots.release()

    def handle_error(self, request, address):
        # Never log request bodies, transcripts or parser exceptions.
        pass


def parse_audio(content_type, body, languages):
    if "\r" in content_type or "\n" in content_type or len(content_type) > 256:
        raise Refused(400, "Invalid multipart content type")
    header = b"Content-Type: " + content_type.encode("ascii") + b"\r\nMIME-Version: 1.0\r\n\r\n"
    declaration = email.parser.BytesHeaderParser(policy=email.policy.default).parsebytes(header)
    boundary = declaration.get_boundary()
    if (declaration.get_content_type() != "multipart/form-data" or not boundary
            or len(boundary) > 70):
        raise Refused(400, "Expected multipart audio")
    marker = b"--" + boundary.encode("ascii")
    # Bound parser object amplification before constructing a MIME tree.
    if not 2 <= body.count(marker) <= 6:
        raise Refused(400, "Too many or missing multipart fields")
    if any(len(part.split(b"\r\n\r\n", 1)[0]) > 2048 for part in body.split(marker)[1:-1]):
        raise Refused(400, "Multipart field headers exceed limit")
    message = email.parser.BytesParser(policy=email.policy.default).parsebytes(header + body)
    if not message.is_multipart() or message.defects:
        raise Refused(400, "Malformed multipart audio")
    fields = {}
    for part in message.iter_parts():
        name = part.get_param("name", header="content-disposition")
        if (name not in ("file", "language", "response_format", "translate", "detect_language")
                or name in fields or part.is_multipart() or part.defects
                or part.get_content_disposition() != "form-data"
                or part.get("Content-Transfer-Encoding") not in (None, "binary")):
            raise Refused(400, "Invalid or duplicate inference field")
        fields[name] = part.get_payload(decode=True)
    if fields.get("response_format", b"json") != b"json":
        raise Refused(422, "Only JSON transcription is supported")
    if any(fields.get(key, b"false") != b"false" for key in ("translate", "detect_language")):
        raise Refused(422, "Translation and automatic language detection are disabled")
    language = fields.get("language", b"").decode("ascii")
    if language not in languages:
        raise Refused(422, "Selected language is unsupported by the loaded model")
    audio = fields.get("file", b"")
    with wave.open(io.BytesIO(audio), "rb") as wav:
        if (wav.getnchannels() != 1 or wav.getsampwidth() != 2 or wav.getframerate() != 16000
                or wav.getcomptype() != "NONE" or not 0 < wav.getnframes() <= MAX_FRAMES):
            raise Refused(422, "Expected bounded mono 16kHz PCM16 audio")
        pcm = wav.readframes(MAX_FRAMES + 1)
        if len(pcm) != wav.getnframes() * 2:
            raise Refused(400, "Truncated audio")
    canonical = io.BytesIO()
    with wave.open(canonical, "wb") as wav:
        wav.setparams((1, 2, 16000, 0, "NONE", "not compressed"))
        wav.writeframes(pcm)
    return language, canonical.getvalue()


def inference_form(language, audio):
    boundary = "murmur-" + secrets.token_hex(24)
    while boundary.encode() in audio:
        boundary = "murmur-" + secrets.token_hex(24)
    fields = [("file", audio), ("language", language.encode("ascii")),
              ("response_format", b"json"), ("translate", b"false"), ("detect_language", b"false")]
    parts = []
    for name, value in fields:
        disposition = f'Content-Disposition: form-data; name="{name}"'
        if name == "file":
            disposition += '; filename="audio.wav"\r\nContent-Type: audio/wav'
        parts.append(f"--{boundary}\r\n{disposition}\r\n\r\n".encode() + value + b"\r\n")
    return boundary, b"".join(parts) + f"--{boundary}--\r\n".encode()


def drop_annotation_lines(text):
    """Remove annotation-only lines; annotations inside spoken text are kept."""
    lines = text.splitlines(keepends=True)
    kept = [line for line in lines if not ANNOTATION_LINE.fullmatch(line)]
    if len(kept) == len(lines):
        return text
    return "".join(kept) if any(line.strip() for line in kept) else ""


class Gateway:
    def __init__(self, port, engine_port, prefix, metadata, model_sha256, is_ready, vad=False):
        # vad: the owned engine runs Silero VAD, so no-speech audio yields "".
        self.capabilities = {"protocol_version": 1, "instance_id": secrets.token_hex(32),
                             "model_sha256": model_sha256,
                             "languages": list(LANGUAGES[:metadata["language_count"]]),
                             "vad": vad}
        self.engine_port, self.prefix = engine_port, prefix
        self.engine_ready = is_ready
        self.failed = threading.Event()
        self.closed = threading.Event()
        self.inference = threading.Lock()
        gateway = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def setup(self):
                super().setup()
                self.connection.settimeout(BODY_SECONDS)

            def reply(self, status, body, bound=False):
                encoded = json.dumps(body).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(encoded)))
                self.send_header("Connection", "close")
                if bound:
                    self.send_header("X-Murmur-Instance", gateway.capabilities["instance_id"])
                    self.send_header("X-Murmur-Model", gateway.capabilities["model_sha256"])
                self.end_headers()
                self.wfile.write(encoded)
                self.close_connection = True

            def do_GET(self):
                if self.path != "/health":
                    self.reply(404, {"error": "Unknown route"})
                elif gateway.closed.is_set() or not gateway.is_ready():
                    self.reply(503, {"status": "unavailable"})
                else:
                    self.reply(200, {"status": "ok", "murmur_capabilities": gateway.capabilities})

            def do_POST(self):
                if self.path != "/inference":
                    self.reply(404, {"error": "Unknown route"})
                    return
                if not gateway.inference.acquire(blocking=False):
                    self.reply(429, {"error": "Local inference is busy; retry after it completes"})
                    return
                try:
                    if gateway.closed.is_set() or not gateway.is_ready():
                        raise Refused(503, "Local model is unavailable")
                    for name, key in (("X-Murmur-Instance", "instance_id"), ("X-Murmur-Model", "model_sha256")):
                        if self.headers.get_all(name) != [gateway.capabilities[key]]:
                            raise Refused(409, "Local instance/model changed; repeat preflight")
                    if self.headers.get_all("Transfer-Encoding") or len(self.headers.get_all("Content-Length", [])) != 1:
                        raise Refused(400, "Expected a bounded Content-Length")
                    size = int(self.headers["Content-Length"])
                    if not 0 < size <= MAX_BODY:
                        raise Refused(413, "Audio request exceeds the capture limit")
                    end = time.monotonic() + BODY_SECONDS
                    body = bytearray()
                    while len(body) < size:
                        remaining = end - time.monotonic()
                        if remaining <= 0:
                            raise Refused(408, "Audio upload timed out")
                        self.connection.settimeout(remaining)
                        chunk = self.rfile.read1(min(65536, size - len(body)))
                        if not chunk:
                            raise Refused(400, "Incomplete audio request")
                        body.extend(chunk)
                    language, audio = parse_audio(self.headers.get("Content-Type", ""), bytes(body), gateway.capabilities["languages"])
                    result = gateway.transcribe(language, audio)
                    if gateway.closed.is_set() or not gateway.is_ready():
                        raise Refused(503, "Local model stopped; result discarded")
                    response = (200, result, True)
                except Refused as error:
                    response = (error.status, {"error": error.message}, False)
                except (ValueError, UnicodeError, EOFError, wave.Error, RecursionError):
                    response = (400, {"error": "Malformed inference request"}, False)
                except (OSError, http.client.HTTPException):
                    response = (502, {"error": "Local inference failed or timed out"}, False)
                finally:
                    gateway.inference.release()
                # Completion frees inference admission before any response bytes
                # become observable. Slow delivery cannot make the next request busy.
                self.reply(*response)

            def log_message(self, *args):
                pass

        self.server = BoundedServer(("127.0.0.1", port), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)
        self.thread.start()

    def is_ready(self):
        return not self.closed.is_set() and not self.failed.is_set() and self.engine_ready()

    def transcribe(self, language, audio):
        boundary, body = inference_form(language, audio)
        end = time.monotonic() + INFERENCE_SECONDS
        connection = http.client.HTTPConnection("127.0.0.1", self.engine_port, timeout=INFERENCE_SECONDS)
        try:
            connection.connect()
            transport = connection.sock
            connection.request("POST", self.prefix + "/inference", body,
                               {"Content-Type": "multipart/form-data; boundary=" + boundary})
            transport.settimeout(max(0.001, end - time.monotonic()))
            response = connection.getresponse()
            if response.status != 200:
                raise Refused(502, "Local engine rejected inference")
            data = bytearray()
            while not response.isclosed():
                remaining = end - time.monotonic()
                if remaining <= 0:
                    raise Refused(504, "Local inference timed out")
                transport.settimeout(remaining)
                chunk = response.read1(min(65536, MAX_RESPONSE + 1 - len(data)))
                if not chunk:
                    break
                data.extend(chunk)
                if len(data) > MAX_RESPONSE:
                    raise Refused(502, "Local engine response exceeds limit")
            try:
                result = json.loads(data)
            except (ValueError, UnicodeError):
                raise Refused(502, "Malformed local engine response") from None
            if not isinstance(result, dict) or not isinstance(result.get("text"), str):
                raise Refused(502, "Local engine response has no transcript")
            return {"text": drop_annotation_lines(result["text"])}
        except TimeoutError:
            self.failed.set()
            raise Refused(504, "Local inference timed out") from None
        except Exception:
            # Closing HTTP does not cancel whisper.cpp's inference. Retire this
            # child after an uncertain completion, never queue work behind it.
            self.failed.set()
            raise
        finally:
            connection.close()

    def close(self):
        self.closed.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=1)
