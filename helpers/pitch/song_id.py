"""Song identification against your own library, offline: nothing leaves the house, and it only knows songs
you have. Like Shazam's landmarks: the loudest points of each song's spectrogram, paired up, become hashes
(frequency, frequency, time apart); a recording that shares many hashes at one consistent time offset with
a song is that song.

Runs in its own container (Prelude sets it up: helpers/pitch), music folder read-only. It fingerprints every song once
(newest files first, low priority; $OUT/fp2/*.npz, one per song), keeps all hashes in memory (about 150 MB for
2,000 songs), and answers Conductor (songid_url) on port 4540:
  POST /identify  (any audio ffmpeg reads, ~5-15 s)  ->  {"path", "offset", "score", "second"} or {"path": null}
  GET  /status    ->  {"songs": n, "hashes": n, "indexing": bool}
"""
import hashlib, json, os, shutil, subprocess, sys, tempfile, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import numpy as np

SR = 8000
NFFT, HOP = 1024, 256            # 128 ms windows, 32 ms steps
NEIGH_T, NEIGH_F = 10, 12        # a peak is the loudest point within ±10 frames, ±12 bins
PEAKS_PER_S = 12                 # stored songs (fewer: memory); a recording gets more (QUERY_PEAKS)
QUERY_PEAKS = 60
FAN, DT_MAX, DF_MAX = 5, 63, 63  # each peak pairs with the next 5 within 2 s and ±63 bins (a recording)
STORE_FAN = 4                    # a stored song: the next 4 (about 9,000 hashes a song)
FP_DIR = "fp2"                   # fingerprints made this way ("fp" had denser ones: too big for 2,000 songs)
MIN_SCORE = 7                    # aligned hashes needed to call it (and twice the next song's, plus 3)
MUSIC = os.environ.get("MUSIC", "/music")
OUT = os.environ.get("OUT", "/out")
PORT = int(os.environ.get("SONG_ID_PORT", "4540"))
GAP = float(os.environ.get("SONG_ID_GAP", "2"))
EXTS = (".mp3", ".flac", ".m4a", ".ogg", ".opus", ".wav", ".aac", ".wma", ".aiff", ".ape", ".mpc")


def log(*a):
    print("song-id:", *a, flush=True)


def decode(path_or_bytes, seconds=None):
    """Mono 8 kHz floats from a file path or bytes (anything ffmpeg reads)."""
    if isinstance(path_or_bytes, (bytes, bytearray)):
        # a recording: through a file, not a pipe. A phone's M4A keeps its index at the end, which ffmpeg can only
        # reach in a file it can seek in ("partial file" from a pipe).
        with tempfile.NamedTemporaryFile(suffix=".rec") as f:
            f.write(path_or_bytes)
            f.flush()
            return decode(f.name, seconds)
    cmd = ["ffmpeg", "-v", "error", "-nostdin", "-i", path_or_bytes]
    if seconds:
        cmd += ["-t", str(seconds)]
    cmd += ["-ac", "1", "-ar", str(SR), "-f", "s16le", "-"]
    r = subprocess.run(cmd, capture_output=True, timeout=300)
    if not r.stdout:
        raise RuntimeError(r.stderr.decode(errors="replace")[:200] or "no audio")
    return np.frombuffer(r.stdout, dtype=np.int16).astype(np.float32) / 32768.0


def spectrogram(x, chunk=1024):
    """Log magnitudes, frame by frame (float32). Done a chunk of frames at a time: the whole 10-minute
    spectrogram in one go took about 450 MB on the way (numpy's FFT works in double precision)."""
    n = (len(x) - NFFT) // HOP + 1
    out = np.zeros((max(n, 0), NFFT // 2 + 1), np.float32)
    if n <= 0:
        return out
    win = np.hanning(NFFT).astype(np.float32)
    frames = np.lib.stride_tricks.sliding_window_view(x, NFFT)[::HOP][:n]   # a view: no copy
    for i in range(0, n, chunk):
        out[i:i + chunk] = np.log(np.abs(np.fft.rfft(frames[i:i + chunk] * win, axis=1)) + 1e-6)
    return out


def _local_max(s):
    """The largest value within ±NEIGH_T frames and ±NEIGH_F bins: one axis at a time, in place on slices
    (no copies the size of the spectrogram, and no wrapping round from the song's end to its start)."""
    m = s.copy()
    for axis, k in ((0, NEIGH_T), (1, NEIGH_F)):
        base = m.copy()
        for d in range(1, k + 1):
            if axis == 0:
                np.maximum(m[d:], base[:-d], out=m[d:])
                np.maximum(m[:-d], base[d:], out=m[:-d])
            else:
                np.maximum(m[:, d:], base[:, :-d], out=m[:, d:])
                np.maximum(m[:, :-d], base[:, d:], out=m[:, :-d])
    return m


def peaks(s, per_s=PEAKS_PER_S):
    """(frame, bin) of the spectrogram's landmarks, about per_s a second, time order."""
    if len(s) == 0:
        return np.zeros((0, 2), np.int32)
    s = s[:, 4:]                                   # below ~30 Hz: rumble
    is_peak = (s == _local_max(s)) & (s > np.median(s) + 1.0)
    t, f = np.nonzero(is_peak)
    if len(t) == 0:
        return np.zeros((0, 2), np.int32)
    keep = int(per_s * len(s) * HOP / SR) + 1
    if len(t) > keep:
        top = np.argsort(s[t, f])[-keep:]
        t, f = t[top], f[top]
    order = np.lexsort((f, t))
    return np.stack([t[order], f[order] + 4], axis=1).astype(np.int32)


def hashes(p, fan=FAN):
    """(hash, time of the first peak) for each pair of nearby peaks."""
    out_h, out_t = [], []
    n = len(p)
    for i in range(n):
        t1, f1 = p[i]
        k = 0
        for j in range(i + 1, n):
            dt = p[j, 0] - t1
            if dt > DT_MAX:
                break
            if dt < 1:
                continue
            df = p[j, 1] - f1
            if abs(df) > DF_MAX:
                continue
            out_h.append((int(f1) << 14) | ((int(df) + 64) << 7) | int(dt))
            out_t.append(int(t1))
            k += 1
            if k >= fan:
                break
    return np.array(out_h, np.uint32), np.array(out_t, np.int32)


def fingerprint(x, per_s=PEAKS_PER_S, fan=STORE_FAN):
    return hashes(peaks(spectrogram(x), per_s), fan)


# ───────────── the index ─────────────
class Index:
    """Every stored hash, sorted, with its song and time: 8 bytes each (about 150 MB for 2,000 songs)."""

    def __init__(self):
        self.lock = threading.Lock()
        self.paths = []                            # song number -> path
        self.h = np.zeros(0, np.uint32)            # all hashes, sorted
        self.song = np.zeros(0, np.uint16)
        self.t = np.zeros(0, np.uint16)            # frames (32 ms): up to 35 minutes
        self.pending = []                          # (path, hashes, times) not merged yet

    def add(self, path, h, t):
        with self.lock:
            self.pending.append((path, h, t))
            if len(self.pending) >= 50:
                self._merge()

    def _merge(self):
        """The new songs go into the sorted arrays where they belong (no sorting everything again, and only
        one array copied at a time, so memory stays near the index's own size)."""
        if not self.pending:
            return
        known = {p: i for i, p in enumerate(self.paths)}
        drop, hs, ss, ts = set(), [], [], []
        for path, h, t in self.pending:
            if path in known:
                drop.add(known[path])              # analysed again (the file changed): the old one goes
            known[path] = len(self.paths)
            self.paths.append(path)
            hs.append(np.asarray(h, np.uint32))
            ss.append(np.full(len(h), known[path], np.uint16))
            ts.append(np.minimum(np.asarray(t), 65535).astype(np.uint16))
        self.pending = []
        if drop and len(self.h):
            keep = ~np.isin(self.song, np.array(sorted(drop), np.uint16))
            self.h, self.song, self.t = self.h[keep], self.song[keep], self.t[keep]
        h, s, t = np.concatenate(hs), np.concatenate(ss), np.concatenate(ts)
        if drop:                                   # a song changed twice before a merge: only its latest
            keep = ~np.isin(s, np.array(sorted(drop), np.uint16))
            h, s, t = h[keep], s[keep], t[keep]
        order = np.argsort(h, kind="stable")
        h, s, t = h[order], s[order], t[order]
        at = np.searchsorted(self.h, h, "right")
        self.h = np.insert(self.h, at, h)
        self.song = np.insert(self.song, at, s)
        self.t = np.insert(self.t, at, t)

    def flush(self):
        with self.lock:
            self._merge()

    def identify(self, qh, qt):
        """(path, offset in s, score, second best) or None."""
        with self.lock:
            self._merge()
            if len(self.h) == 0 or len(qh) == 0:
                return None
            lo = np.searchsorted(self.h, qh, "left")
            hi = np.searchsorted(self.h, qh, "right")
            counts = hi - lo
            idx = np.concatenate([np.arange(a, b) for a, b in zip(lo, hi) if b > a] or [np.zeros(0, np.int64)])
            if len(idx) == 0:
                return None
            q_t = np.repeat(qt, counts)
            song = self.song[idx]
            offset = self.t[idx].astype(np.int32) - q_t
            key = song.astype(np.int64) * 200000 + (offset + 100000)
            vals, n = np.unique(key, return_counts=True)
            # the best offset of each song, then the best song
            best = {}
            for v, c in zip(vals, n):
                sng = int(v // 200000)
                if c > best.get(sng, (0, 0))[0]:
                    best[sng] = (int(c), int(v % 200000) - 100000)
            ranked = sorted(best.items(), key=lambda kv: -kv[1][0])
            (sng, (score, off)) = ranked[0]
            second = ranked[1][1][0] if len(ranked) > 1 else 0
            return self.paths[sng], max(0.0, off * HOP / SR), score, second


INDEX = Index()
STATUS = {"indexing": False}


def fp_file(rel):
    return os.path.join(OUT, FP_DIR, hashlib.sha1(rel.encode()).hexdigest() + ".npz")


def load_saved():
    n = 0
    old = os.path.join(OUT, "fp")                  # the denser first fingerprints: made again, smaller
    if os.path.isdir(old):
        shutil.rmtree(old, ignore_errors=True)
    d = os.path.join(OUT, FP_DIR)
    for name in os.listdir(d) if os.path.isdir(d) else []:
        try:
            z = np.load(os.path.join(d, name), allow_pickle=False)
            rel = str(z["path"])
            if os.path.exists(os.path.join(MUSIC, rel)):
                INDEX.add(rel, z["h"], z["t"])
                n += 1
        except Exception:
            pass
    INDEX.flush()
    return n


def library():
    """Every song, newest files first."""
    files = []
    for root, _, names in os.walk(MUSIC):
        for n in names:
            if n.lower().endswith(EXTS):
                full = os.path.join(root, n)
                try:
                    files.append((os.path.getmtime(full), os.path.relpath(full, MUSIC)))
                except OSError:
                    pass
    return [rel for _, rel in sorted(files, reverse=True)]


def indexer():
    os.makedirs(os.path.join(OUT, FP_DIR), exist_ok=True)
    try:
        os.nice(10)
    except OSError:
        pass
    log(f"{load_saved()} songs fingerprinted before")
    while True:
        STATUS["indexing"] = True
        done = 0
        for rel in library():
            out = fp_file(rel)
            full = os.path.join(MUSIC, rel)
            try:
                if os.path.exists(out) and os.path.getmtime(out) >= os.path.getmtime(full):
                    continue
                h, t = fingerprint(decode(full, seconds=600))
                tmp = out + ".tmp.npz"
                np.savez(tmp, path=np.array(rel), h=h, t=t)
                os.replace(tmp, out)
                INDEX.add(rel, h, t)
                done += 1
            except Exception as e:
                log(f"{rel}: {str(e)[:100]}")
            time.sleep(GAP)
        INDEX.flush()
        STATUS["indexing"] = False
        if done:
            log(f"+{done} songs; {len(INDEX.paths)} in all")
        time.sleep(3600)  # new songs are looked for every hour


def identify(audio):
    qh, qt = fingerprint(decode(audio, seconds=30), QUERY_PEAKS, FAN)
    r = INDEX.identify(qh, qt)
    if not r:
        return {"path": None, "score": 0}
    path, off, score, second = r
    # enough hashes in line, and clearly more than any other song
    if score < MIN_SCORE or score < 2 * second + 3:
        return {"path": None, "score": score, "second": second}
    return {"path": path, "offset": round(off, 1), "score": score, "second": second}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def reply(self, obj, code=200):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path.startswith("/status"):
            return self.reply({"songs": len(INDEX.paths), "hashes": int(len(INDEX.h)), "indexing": STATUS["indexing"],
                               **({"error": STATUS["error"]} if STATUS.get("error") else {})})
        self.reply({"error": "not here"}, 404)

    def do_POST(self):
        if not self.path.startswith("/identify"):
            return self.reply({"error": "not here"}, 404)
        n = int(self.headers.get("Content-Length") or 0)
        if not 0 < n <= 8 * 1024 * 1024:
            return self.reply({"error": "send 5-15 s of audio"}, 400)
        try:
            self.reply(identify(self.rfile.read(n)))
        except Exception as e:
            self.reply({"error": str(e)[:200]}, 400)


def keep_indexing():
    """The indexer, started again after a failure (a data folder it can't write yet, the drive away): the
    server would otherwise keep answering with an index that never grows."""
    while True:
        try:
            indexer()
        except Exception as e:
            STATUS["indexing"] = False
            STATUS["error"] = str(e)[:200]
            log("indexing stopped, trying again in a minute:", e)
            time.sleep(60)


def main():
    threading.Thread(target=keep_indexing, daemon=True).start()
    srv = ThreadingHTTPServer(("0.0.0.0", PORT), Handler)
    log(f"listening on :{PORT}")
    srv.serve_forever()


if __name__ == "__main__":
    if len(sys.argv) > 2 and sys.argv[1] == "test":       # song_id.py test SONG... CLIP: index songs, find the clip
        for p in sys.argv[2:-1]:
            INDEX.add(p, *fingerprint(decode(p, seconds=600)))
        print(identify(open(sys.argv[-1], "rb").read()))
    else:
        main()
