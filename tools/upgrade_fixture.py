#!/usr/bin/env python3
"""Make an upgrade fixture: run an older Conductor (or Maestro) release on a tiny library, fill it through its own API
(playlists, a smart playlist, stars, a rating, plays, a play queue, a radio station, a bookmark, a second user), and
keep its database, its password key and what it answered. tests/upgrade.rs opens each fixture with the current code
and checks every answer is still the same.

    python3 tools/upgrade_fixture.py BINARY PREFIX NAME
      BINARY  the old release's program (built from its tag: git worktree add … vX && cargo build --release)
      PREFIX  its settings' prefix: CONDUCTOR, or MAESTRO for 0.1
      NAME    the fixture's folder under tests/fixtures/upgrade/ (the tag)

Needs ffmpeg. Everything in it is made up (test user, test password, sine-wave songs).
"""
import hashlib, json, os, shutil, socket, subprocess, sys, tempfile, time, urllib.parse, urllib.request

BIN, PREFIX, NAME = sys.argv[1], sys.argv[2], sys.argv[3]
OUT = os.path.join(os.path.dirname(__file__), "..", "tests", "fixtures", "upgrade", NAME)
USER, PASS = "tester", "s3cret pass"
SONGS = [("Alpha/First Album/01 One.flac", "One", "Alpha", "First Album", 1), ("Alpha/First Album/02 Two.flac", "Two", "Alpha", "First Album", 2),
         ("The Beta Band/Second/01 Three.mp3", "Three", "The Beta Band", "Second", 1)]

tmp = tempfile.mkdtemp()
music, data = os.path.join(tmp, "music"), os.path.join(tmp, "data")
for rel, title, artist, album, n in SONGS:
    p = os.path.join(music, rel)
    os.makedirs(os.path.dirname(p), exist_ok=True)
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-f", "lavfi", "-i", f"sine=frequency={220 * n}:duration=3", "-metadata", f"title={title}",
                    "-metadata", f"artist={artist}", "-metadata", f"album={album}", "-metadata", f"track={n}", "-metadata", "date=2021",
                    "-metadata", "genre=Test", p], check=True)
s = socket.socket(); s.bind(("127.0.0.1", 0)); port = s.getsockname()[1]; s.close()
os.makedirs(data)
cfg = os.path.join(data, "config.toml")
open(cfg, "w").write(f'data_dir = "{data}"\nlisten = "127.0.0.1:{port}"\njam_listen = ""\ncatalog = false\nlyrics_online = false\n'
                     f'covers_fetch = false\nconcerts = false\ninfo_online = false\n[[libraries]]\nname = "Music"\npath = "{music}"\n')
env = dict(os.environ, **{f"{PREFIX}_ADMIN_USER": USER, f"{PREFIX}_ADMIN_PASSWORD": PASS, f"{PREFIX}_PASSWORD": "second pass"})
srv = subprocess.Popen([BIN, "--config", cfg], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
base = f"http://127.0.0.1:{port}"


def call(m, **p):
    q = {"u": USER, "t": hashlib.md5((PASS + "s4lt").encode()).hexdigest(), "s": "s4lt", "v": "1.16.1", "c": "fixture", "f": "json", **p}
    with urllib.request.urlopen(f"{base}/rest/{m}?" + urllib.parse.urlencode(q, doseq=True), timeout=30) as r:
        v = json.load(r)["subsonic-response"]
    assert v["status"] == "ok", (m, v)
    return v


try:
    for _ in range(100):
        try:
            if call("ping") and call("search3", query="")["searchResult3"].get("song"):
                break
        except Exception:
            pass
        time.sleep(0.2)
    songs = sorted(call("search3", query="")["searchResult3"]["song"], key=lambda x: x["title"])
    one, three, two = songs[0]["id"], songs[1]["id"], songs[2]["id"]  # One, Three, Two
    album = call("getSong", id=one)["song"]["albumId"]
    # a second user, by the command line (as people do)
    subprocess.run([BIN, "--config", cfg, "user", "add", "second"], env=env, check=True, capture_output=True)
    call("createPlaylist", name="Road trip", songId=[two, one])
    call("star", id=one); call("star", albumId=album)
    call("setRating", id=two, rating=4)
    for t in (1700000000, 1700000600):
        call("scrobble", id=one, submission="true", time=t * 1000)
    call("savePlayQueue", id=[one, two, three], current=two, position=1234)
    call("createInternetRadioStation", streamUrl="http://radio.example/stream", name="Example FM")
    call("createBookmark", id=three, position=2000, comment="here")
    # a smart playlist through the Navidrome stand-in (what Encore uses)
    tok = json.load(urllib.request.urlopen(urllib.request.Request(f"{base}/auth/login", data=json.dumps({"username": USER, "password": PASS}).encode(),
                                                                  headers={"Content-Type": "application/json"}), timeout=30))["token"]
    rules = {"all": [{"is": {"loved": True}}], "sort": "title", "limit": 10}
    urllib.request.urlopen(urllib.request.Request(f"{base}/api/playlist", data=json.dumps({"name": "Hearted", "rules": rules, "public": False}).encode(),
                                                  headers={"Content-Type": "application/json", "X-ND-Authorization": f"Bearer {tok}"}), timeout=30)
    # what it answers now: the upgraded server must answer the same
    pls = {p["name"]: p for p in call("getPlaylists")["playlists"]["playlist"]}
    expected = {
        "made_by": NAME,
        "songs": {x["title"]: x["id"] for x in songs},
        "album_one": album,
        "playlists": {n: {"songCount": p["songCount"], "songs": [e["id"] for e in call("getPlaylist", id=p["id"])["playlist"].get("entry", [])]}
                      for n, p in pls.items()},
        "starred_songs": [x["id"] for x in call("getStarred2")["starred2"].get("song", [])],
        "starred_albums": [x["id"] for x in call("getStarred2")["starred2"].get("album", [])],
        "rating_two": call("getSong", id=two)["song"].get("userRating"),
        "plays_one": call("getSong", id=one)["song"].get("playCount"),
        "queue": {k: v for k, v in call("getPlayQueue")["playQueue"].items() if k in ("current", "position")},
        "queue_songs": [e["id"] for e in call("getPlayQueue")["playQueue"]["entry"]],
        "radio": [r["name"] for r in call("getInternetRadioStations")["internetRadioStations"]["internetRadioStation"]],
        "bookmarks": [(b["entry"]["id"], b["position"]) for b in call("getBookmarks")["bookmarks"]["bookmark"]],
        "users": [USER, "second"],
    }
finally:
    srv.terminate(); srv.wait(timeout=30)

shutil.rmtree(OUT, ignore_errors=True)
os.makedirs(OUT)
for f in os.listdir(data):
    if f.endswith((".db", ".db-wal", ".db-shm")) or f == "secret.key":
        shutil.copy(os.path.join(data, f), os.path.join(OUT, f))
# one file: what's still in the write-ahead log goes into the database (git keeps only *.db, not -wal / -shm)
import sqlite3
for f in os.listdir(OUT):
    if f.endswith(".db"):
        c = sqlite3.connect(os.path.join(OUT, f))
        c.execute("PRAGMA wal_checkpoint(TRUNCATE)")
        c.execute("PRAGMA journal_mode=DELETE")
        c.close()
for f in os.listdir(OUT):
    if f.endswith(("-wal", "-shm")):
        os.remove(os.path.join(OUT, f))
json.dump(expected, open(os.path.join(OUT, "expected.json"), "w"), indent=1, sort_keys=True)
shutil.rmtree(tmp)
print("made", OUT, sorted(os.listdir(OUT)))
