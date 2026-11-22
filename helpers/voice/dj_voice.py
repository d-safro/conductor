"""The DJ's voice: Piper (offline text-to-speech) behind a tiny HTTP server, for Conductor's DJ (Interlude, dj_voice_url).
Runs in its own container (Prelude sets it up: helpers/voice), only when AI is on. The voice model is downloaded once into
$VOICES (default en_US-lessac-medium, about 60 MB).

  POST /speak  {"text": "...", "voice": optional}  ->  audio/wav
  GET  /status ->  {"voice": name, "ready": bool}
"""
import json, os, shutil, subprocess, sys, tempfile, threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

VOICES = os.environ.get("VOICES", "/voices")
VOICE = os.environ.get("DJ_VOICE", "en_US-lessac-medium")
PORT = int(os.environ.get("DJ_VOICE_PORT", "4541"))
_lock = threading.Lock()  # one at a time: it's quick, and it keeps memory low


def log(*a):
    print("dj-voice:", *a, flush=True)


def model(name=VOICE):
    return os.path.join(VOICES, name + ".onnx")


def ready(name=VOICE):
    return os.path.exists(model(name)) and os.path.exists(model(name) + ".json")


_download = threading.Lock()


def ensure_voice(name=VOICE):
    """Download the voice the first time (Piper's own downloader, from the official voices repository). Into a
    folder of its own first, then moved into place: a half-downloaded voice never looks ready (Piper would fail on
    it), and a request that comes during the download waits for it."""
    if ready(name):
        return True
    with _download:
        if ready(name):
            return True
        os.makedirs(VOICES, exist_ok=True)
        part = os.path.join(VOICES, ".download-" + name)
        shutil.rmtree(part, ignore_errors=True)
        os.makedirs(part)
        r = subprocess.run([sys.executable, "-m", "piper.download_voices", "--data-dir", part, name],
                           capture_output=True, text=True, timeout=600)
        if r.returncode != 0:
            log("couldn't download the voice:", (r.stderr or r.stdout)[-300:])
        else:
            # the settings first, the model last: ready() needs both
            for f in [name + ".onnx.json", name + ".onnx"]:
                if os.path.exists(os.path.join(part, f)):
                    os.replace(os.path.join(part, f), os.path.join(VOICES, f))
        shutil.rmtree(part, ignore_errors=True)
    return ready(name)


def speak(text, name=VOICE):
    text = " ".join(str(text).split())[:600]
    if not text:
        raise ValueError("nothing to say")
    if not ensure_voice(name):
        raise RuntimeError("no voice")
    with _lock, tempfile.NamedTemporaryFile(suffix=".wav") as out:
        r = subprocess.run([sys.executable, "-m", "piper", "--model", model(name), "--config", model(name) + ".json",
                            "--output_file", out.name],
                           input=text, capture_output=True, text=True, timeout=120)
        if r.returncode != 0:
            raise RuntimeError((r.stderr or "piper failed")[-300:])
        return open(out.name, "rb").read()


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def reply(self, code, body, ctype="application/json"):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path.startswith("/status"):
            return self.reply(200, json.dumps({"voice": VOICE, "ready": ready()}).encode())
        self.reply(404, b"{}")

    def do_POST(self):
        if not self.path.startswith("/speak"):
            return self.reply(404, b"{}")
        try:
            n = int(self.headers.get("Content-Length") or 0)
            j = json.loads(self.rfile.read(min(n, 8192)) or b"{}")
            name = j.get("voice") or VOICE
            if not all(c.isalnum() or c in "_-" for c in name):
                return self.reply(400, b'{"error": "bad voice"}')
            self.reply(200, speak(j.get("text", ""), name), "audio/wav")
        except Exception as e:
            self.reply(500, json.dumps({"error": str(e)[:200]}).encode())


if __name__ == "__main__":
    threading.Thread(target=ensure_voice, daemon=True).start()
    log(f"listening on :{PORT} ({VOICE})")
    ThreadingHTTPServer(("0.0.0.0", PORT), Handler).serve_forever()
