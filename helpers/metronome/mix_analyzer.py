"""Automix analysis: for each song, where its sound starts and ends, where the intro ends and the outro starts,
its tempo and first beat, so Encore can start the next song at the right moment (Conductor serves it: mixinfo_dir).

Runs in its own container (Prelude sets it up: helpers/metronome) with the music folder read-only. Decodes with ffmpeg
(mono, 11025 Hz) and measures with numpy. Songs asked for (Conductor writes their paths to
$OUT/wanted.txt, the songs coming up in your queue first) go first; then, slowly, the rest of the library
(MIX_ANALYZE_ALL=0 turns that off; MIX_ANALYZE_GAP seconds between songs, so the disk stays free for playing).
Results: one JSON line per song in $OUT/info.jsonl:
  {"path", "mtime", "dur", "start", "introEnd", "outroStart", "end", "bpm", "beat", "endDb"}  (seconds, dB)
"""
import json, os, subprocess, sys, time

import numpy as np

SR = 11025
HOP = 256                       # ~23 ms
MUSIC = os.environ.get("MUSIC", "/music")
OUT = os.environ.get("OUT", "/out")
ALL = os.environ.get("MIX_ANALYZE_ALL", "1") != "0"
GAP = float(os.environ.get("MIX_ANALYZE_GAP", "4"))
EXTS = (".mp3", ".flac", ".m4a", ".ogg", ".opus", ".wav", ".aac", ".wma", ".alac", ".aiff", ".ape", ".mpc")
SILENT_DB = -48.0               # quieter than this (per frame) is silence


def log(*a):
    print("mix-analyzer:", *a, flush=True)


def decode(path, seconds=None):
    cmd = ["ffmpeg", "-v", "error", "-nostdin", "-i", path, "-ac", "1", "-ar", str(SR), "-f", "s16le", "-"]
    if seconds:
        cmd[5:5] = ["-t", str(seconds)]
    r = subprocess.run(cmd, capture_output=True, timeout=300)
    if r.returncode != 0 and not r.stdout:
        raise RuntimeError(r.stderr.decode(errors="replace")[:200])
    return np.frombuffer(r.stdout, dtype=np.int16).astype(np.float32) / 32768.0


def frame_db(x):
    n = len(x) // HOP
    if n == 0:
        return np.full(1, -120.0)
    f = x[: n * HOP].reshape(n, HOP)
    rms = np.sqrt(np.mean(f * f, axis=1) + 1e-12)
    return 20 * np.log10(rms + 1e-9)


def smooth(v, k):
    if k <= 1 or len(v) < k:
        return v
    return np.convolve(v, np.ones(k) / k, mode="same")


def tempo(x):
    """(bpm, first beat in seconds) from the onset envelope's autocorrelation, or (None, None)."""
    n = len(x) // HOP
    if n < 400:
        return None, None
    win = np.hanning(HOP * 2).astype(np.float32)
    frames = np.lib.stride_tricks.sliding_window_view(x[: n * HOP + HOP], HOP * 2)[::HOP][:n] * win
    mag = np.log1p(10 * np.abs(np.fft.rfft(frames, axis=1)))
    flux = np.maximum(0, np.diff(mag, axis=0)).sum(axis=1)
    flux = flux - smooth(flux, 16)
    flux = np.maximum(flux, 0)
    if flux.std() < 1e-6:
        return None, None
    fps = SR / HOP
    lo, hi = int(fps * 60 / 180), int(fps * 60 / 60)        # 60..180 BPM
    seg = flux[: int(fps * 120)]                              # the first two minutes are enough
    ac = np.correlate(seg, seg, mode="full")[len(seg) - 1:]
    lags = np.arange(lo, hi + 1)
    # a gentle preference for 90–140 BPM, where most songs are counted
    bpms = 60 * fps / lags
    weight = np.exp(-0.5 * (np.log2(bpms / 115) / 0.9) ** 2)
    best = lags[np.argmax(ac[lo: hi + 1] * weight)]
    # refine with the neighbours (parabola)
    if lo < best < hi:
        a, b, c = ac[best - 1], ac[best], ac[best + 1]
        d = (a - c) / (2 * (a - 2 * b + c)) if (a - 2 * b + c) != 0 else 0
        period = best + d
    else:
        period = float(best)
    bpm = 60 * fps / period
    # phase: the offset whose beat grid lines up with the most onset energy
    k = int(round(period))
    phases = [seg[p::k].sum() if k > 0 else 0 for p in range(k)]
    beat = int(np.argmax(phases)) / fps
    return round(float(bpm), 1), round(float(beat), 2)


def analyze(path):
    x = decode(path)
    if len(x) < SR * 5:
        raise RuntimeError("too short")
    dur = len(x) / SR
    db = smooth(frame_db(x), 9)                               # ~0.2 s
    t = np.arange(len(db)) * HOP / SR
    sound = np.where(db > SILENT_DB)[0]
    if len(sound) == 0:
        raise RuntimeError("silent")
    start, end = float(t[sound[0]]), float(t[sound[-1]] + HOP / SR)
    loud = float(np.percentile(db[sound], 75))                # the song's usual loudness
    body = np.where(db > loud - 10)[0]                        # "really playing": within 10 dB of that
    intro_end = float(t[body[0]]) if len(body) else start
    outro_start = float(t[body[-1]]) if len(body) else end
    # an intro or outro of more than a third of the song isn't one (a quiet song): no intro / outro then
    if intro_end - start > dur / 3:
        intro_end = start
    if end - outro_start > dur / 3:
        outro_start = end
    tail = db[max(0, sound[-1] - int(10 * SR / HOP)): sound[-1] + 1]
    # the tempo comes from the first two minutes: only those go in (a 25-minute song needed 740 MB before)
    bpm, beat = tempo(x[int(start * SR): int((start + 130) * SR)])
    return {"dur": round(dur, 2), "start": round(start, 2), "introEnd": round(intro_end, 2),
            "outroStart": round(outro_start, 2), "end": round(end, 2), "bpm": bpm,
            "beat": None if beat is None else round(beat + start, 2), "endDb": round(float(np.mean(tail)), 1)}


# ───────────── the loop ─────────────
def done_paths():
    out = {}
    try:
        with open(os.path.join(OUT, "info.jsonl"), encoding="utf-8") as f:
            for line in f:
                try:
                    j = json.loads(line)
                    out[j["path"]] = j.get("mtime", 0)
                except Exception:
                    pass
    except FileNotFoundError:
        pass
    return out


def wanted():
    """Paths Conductor asked for, newest first (it rewrites the file; the analyzer only reads it)."""
    try:
        with open(os.path.join(OUT, "wanted.txt"), encoding="utf-8") as f:
            lines = [l.strip() for l in f if l.strip()]
    except FileNotFoundError:
        return []
    return list(dict.fromkeys(reversed(lines)))


def library():
    for root, dirs, files in os.walk(MUSIC):
        dirs.sort()
        for n in sorted(files):
            if n.lower().endswith(EXTS):
                yield os.path.relpath(os.path.join(root, n), MUSIC)


def save(rel, mtime, info):
    with open(os.path.join(OUT, "info.jsonl"), "a", encoding="utf-8") as f:
        f.write(json.dumps({"path": rel, "mtime": mtime, **info}) + "\n")


def doing_file():
    return os.path.join(OUT, "doing.txt")


def note_crash(done):
    """The analyzer was killed mid-song last time (out of memory, a broken file): that song gets an error,
    so it isn't tried again and again."""
    try:
        with open(doing_file(), encoding="utf-8") as f:
            rel, mtime = f.read().rsplit("\n", 1)
    except (OSError, ValueError):
        return
    if done.get(rel) != int(mtime):
        save(rel, int(mtime), {"error": "the analyzer stopped while measuring it"})
        done[rel] = int(mtime)
        log(f"skipping {rel}: it stopped the analyzer last time")
    os.remove(doing_file())


def one(rel, done):
    full = os.path.join(MUSIC, rel)
    try:
        mtime = int(os.path.getmtime(full))
    except OSError:
        return False
    if done.get(rel) == mtime:
        return False
    with open(doing_file(), "w", encoding="utf-8") as f:
        f.write(f"{rel}\n{mtime}")
    try:
        info = analyze(full)
    except Exception as e:
        info = {"error": str(e)[:120]}
    save(rel, mtime, info)
    os.remove(doing_file())
    done[rel] = mtime
    return True


def main():
    os.makedirs(OUT, exist_ok=True)
    try:
        os.nice(10)
    except OSError:
        pass
    done = done_paths()
    note_crash(done)
    log(f"{len(done)} songs analysed before")
    walk = library() if ALL else iter(())
    while True:
        did = False
        for rel in wanted()[:50]:
            if one(rel, done):
                did = True
        if not did:
            rel = next(walk, None)
            if rel is None:
                time.sleep(30)
                walk = library() if ALL else iter(())
                continue
            if one(rel, done):
                time.sleep(GAP)
                continue
        time.sleep(0.5 if did else 0)


if __name__ == "__main__":
    if len(sys.argv) > 1:                                     # mix_analyzer.py FILE...: print what it finds
        for p in sys.argv[1:]:
            print(p, json.dumps(analyze(p)))
    else:
        main()
