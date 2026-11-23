#!/usr/bin/env python3
"""Writes docs/api/<extension>.md, the spec of each of Conductor's own API extensions (the `encore.*` ones in
getOpenSubsonicExtensions), from three places:
  - what each extension is for and notes on its calls: EXTENSIONS below (edit here, not in the .md files);
  - each call's parameters: read from its handler in src/ (c.p.req = required; c.p.get / int / bool / all = optional);
  - every field of each call's answer, with its types: docs/api/shapes/<call>.json (recorded by the tests).

    python3 tools/api_docs.py          write them
    python3 tools/api_docs.py --check  fail if one isn't up to date (CI)
"""
import json, os, re, sys

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
SHAPES = os.path.join(ROOT, "docs", "api", "shapes")

# extension: (what it is, [(call, note)], extra text)
EXTENSIONS = {
    "encore.devices": ("Your devices see each other, and one can play on another (remote control). Each device says "
                       "hello and its state; the others list them and send commands, which the device picks up by "
                       "polling (a long-poll).",
                       [("deviceHello", "A device announces itself (and is listed for a while after)."),
                        ("deviceState", "What it's playing, and where."),
                        ("deviceList", "Your devices, the others' state."),
                        ("deviceCommand", "Send a command (play, pause, next, seek, volume…) to a device."),
                        ("devicePoll", "Wait for commands for this device (answers at once when one comes)."),
                        ("deviceBye", "The device goes away.")], ""),
    "encore.sync": ("Settings and small lists kept the same on all your devices: newest change wins, sets merge.",
                    [("syncGet", "What's stored, with when each part changed."), ("syncSet", "Store a part.")], ""),
    "encore.pairing": ("Sign in on a new device with a code shown on it, approved on a device that's signed in. "
                       "pairStart and pairPoll need no login (the new device has none yet); at most 5 codes wait from "
                       "one address.",
                       [("pairStart", "The new device gets a code to show, and a token to poll with."),
                        ("pairPoll", "Wait for the approval: waiting, then done with the login."),
                        ("pairApprove", "A signed-in device approves the code, handing over a login.")], ""),
    "encore.jam": ("Ensemble: listen together. The host starts a Jam; guests open a link on the separate Jam "
                   "listener (`jam_listen`, `jam_public_url`), search, add and reorder songs.",
                   [("jamStart", "Start one; its link and token."), ("jamState", "The host's queue and position, "
                    "sent for the guests."), ("jamPoll", "Wait for what guests did."), ("jamEnd", "End it: the link "
                    "stops working.")], ""),
    "encore.stats": ("Listening stats from your plays, and the real folders.",
                     [("getListeningStats", "period = week | month | year | all, offset back in time; minutes, "
                       "plays, top songs, artists, albums, genres, by day and hour (local time)."),
                      ("getFolder", "A folder of the music as it is on disk.")], ""),
    "encore.review": ("Curtain Call: the year or month in review.",
                      [("getListeningReview", "period = year | month, offset back in time: top fives, genres, "
                        "first song, longest streak, peak hour, discoveries.")], ""),
    "encore.catalog": ("Stagehand: songs you don't have, from Deezer, marked ⬇ / ⏳ / ✕. They appear in search3, "
                       "getAlbum and getArtist answers with `dz-…` ids (accepted everywhere an id is); hearting or "
                       "adding one asks for it. Only with a downloader (or `catalog = true`).",
                       [("lookupSongs", "Where songs asked for are (status, and the song id once here); "
                         "fetch=true asks for them."),
                        ("getDiscoveries", "Songs you might like that you don't have."),
                        ("getSongDiscoveries", "The same, around one song.")], ""),
    "encore.sonic": ("AudioMuse-AI's sound-alikes, and Encore's own AudioMuse calls passed through "
                     "(`/rest/audiomuse/<endpoint>`, an allow-list, read-only; the login is never passed on).",
                     [("getSonicSimilarTracks", "Songs that sound like this one."),
                      ("findSonicPath", "A path of songs from one to another.")], ""),
    "encore.collabPlaylists": ("Chorus: friends add songs to your playlist through a link on the Jam listener "
                               "(`/jam/pl/<token>`); they can take out only their own. Links last 30 days.",
                               [("playlistShareStart", "A link for a playlist."), ("playlistShareEnd", "Stop it."),
                                ("playlistShares", "Your playlists' links.")], ""),
    "encore.mixes": ("Repertoire: your mixes, as real playlists, per user: Discover Weekly, Release Radar, Daily Mix "
                     "1–6, Daylist.",
                     [("getMixes", "Which kinds are on, and their playlists."),
                      ("setMix", "kind, enabled: turn one on (made at once) or off (its playlists go)."),
                      ("refreshMix", "kind: make it again now.")], ""),
    "encore.concerts": ("Marquee: upcoming shows of the artists you play and follow, near the town set in Encore.",
                        [("getConcerts", "The shows found."), ("refreshConcerts", "Look again now.")], ""),
    "encore.lyricsTranslation": ("Libretto: lyrics translated or romanized line by line by a local Ollama "
                                 "(`ollama_url`). Answers \"ai-off\" without one.",
                                 [("translateLyrics", "id, lang, mode = translate | romanize, line (each line).")], ""),
    "encore.dj": ("Interlude: a line or two about the next song, written by Ollama and read by Piper. Answers "
                  "\"ai-off\" without them.",
                  [("getDjLine", "next, prev, lang: the line, its id and length."),
                   ("djAudio", "id: the line read aloud (WAV; a file, so no answer shape).")], ""),
    "encore.import": ("Transpose: bring in liked songs, playlists, plays and more from other services' exports or "
                      "accounts. You see what would change first; each import can be undone.",
                      [("importUpload", "POST, the export as the body, name = its file name (no answer shape "
                        "recorded: see getImport)."),
                       ("importOnline", "source = spotify | lastfm | deezer, account: read from the service."),
                       ("getImports", "Your imports."), ("getImport", "id: one, with its report."),
                       ("startImport", "id, kinds, askMissing: bring it in."), ("undoImport", "id: take it back."),
                       ("deleteImport", "id: forget it.")], ""),
    "encore.songId": ("Pitch: what's playing? A short recording is matched against your own library "
                      "(`songid_url`).", [("identifySong", "POST, the recording as the body.")], ""),
    "encore.automix": ("Automix: each song's intro, outro and tempo, from the mix analyzer (`mixinfo_dir`).",
                       [("getMixInfo", "id (again for more).")], ""),
    "encore.scrobbling": ("Each user's own ListenBrainz and Last.fm. Tokens and session keys are stored encrypted; "
                          "send the ListenBrainz token as a form POST, not in the address.",
                          [("getScrobbling", "What's linked."),
                           ("setListenBrainz", "token (empty: unlink); checked with ListenBrainz."),
                           ("lastfmLinkStart", "The page to approve on, and the token to finish with (needs "
                            "`lastfm_api_key` and `lastfm_secret`)."),
                           ("lastfmLinkFinish", "token: waiting | linked | expired. Ask every few seconds while "
                            "waiting; only the user who started can finish."),
                           ("lastfmUnlink", "Unlink Last.fm.")], ""),
    "encore.forYou": ("\"Made for you\" without AudioMuse-AI: a mix from the artists you play most and the artists "
                      "that go with them (Conductor's own \"sounds like\"), about 60% artists new to you and 40% your "
                      "favourites' songs you haven't played in two weeks; the same artist never twice in a row. The same "
                      "all day, a new one each day. Empty until something has been played or hearted.",
                      [("getForYou", "count (default 50, at most 200).")], ""),
    "encore.smartPlaylists": ("Smart playlists in Navidrome's rule format (all / any, is, contains, gt, lt, "
                              "inTheRange, inTheLast, before / after, inPlaylist…; sort, order, limit). Not Subsonic "
                              "calls: through the Navidrome stand-in Encore uses.", [],
                              "- `POST /auth/login` (JSON: username, password) → a token for the calls below "
                              "(`X-ND-Authorization: Bearer <token>`).\n"
                              "- `POST /api/playlist` (JSON: name, rules, public) makes one; `PUT /api/playlist/{id}` "
                              "changes it; `GET /api/playlist/{id}` gives its rules.\n"
                              "- They refill when read (at most once a minute) and can't be edited by hand "
                              "(`readonly` in getPlaylist). `inPlaylist` only sees your own playlists and public ones.\n"),
    "encore.playlistImages": ("A playlist's own picture. Through the Navidrome stand-in, like smart playlists.", [],
                              "- `POST /api/playlist/{id}/image` (the picture as the body) and `DELETE` the same.\n"
                              "- The picture is served by getCoverArt as `pl-<id>_…`.\n"),
}

DISPATCH_FILES = ["src/api/mod.rs", "src/api/catalog_api.rs"]


def read(p):
    return open(os.path.join(ROOT, p), encoding="utf-8").read()


def handlers():
    """call name -> (module path, function name)"""
    out = {}
    for f in DISPATCH_FILES:
        for m in re.finditer(r'^\s*"([a-zA-Z0-9]+)"(?:\s*\|\s*"[a-zA-Z0-9]+")*\s*=>\s*([a-zA-Z_:]+)\(', read(f), re.M):
            path = m.group(2)
            parts = path.split("::")
            fn = parts[-1]
            module = parts[-2] if len(parts) > 1 else os.path.splitext(os.path.basename(f))[0]
            out[m.group(1)] = (module, fn)
        for alt in re.finditer(r'^\s*"([a-zA-Z0-9]+)"((?:\s*\|\s*"[a-zA-Z0-9]+")+)\s*=>\s*([a-zA-Z_:]+)\(', read(f), re.M):
            for n in re.findall(r'"([a-zA-Z0-9]+)"', alt.group(2)):
                parts = alt.group(3).split("::")
                out[n] = (parts[-2] if len(parts) > 1 else "mod", parts[-1])
    return out


def body(module, fn):
    for dirpath, _, files in os.walk(os.path.join(ROOT, "src")):
        for f in files:
            if f == module + ".rs" or (module == "mod" and f == "mod.rs"):
                text = open(os.path.join(dirpath, f), encoding="utf-8").read()
                m = re.search(r'^(pub(\(crate\))? )?(async )?fn ' + re.escape(fn) + r'\b', text, re.M)
                if m:
                    rest = text[m.end():]
                    end = re.search(r'^(pub(\(crate\))? )?(async )?fn |^#\[|^mod |^impl ', rest, re.M)
                    return rest[: end.start() if end else len(rest)]
    return ""


def params(call, hs):
    if call not in hs:
        return []
    b = body(*hs[call])
    seen = {}
    for kind, name in re.findall(r'\.p\.(req|get|int|bool|all|float|i64)\("([a-zA-Z_]+)"\)', b):
        seen[name] = seen.get(name) == "required" or kind == "req"
    return [(n, "required" if r else "optional") for n, r in seen.items()]


def shape(call):
    p = os.path.join(SHAPES, call + ".json")
    return json.load(open(p)) if os.path.exists(p) else None


def page(ext, hs):
    what, calls, extra = EXTENSIONS[ext]
    out = [f"# `{ext}`", "", "<!-- Written by tools/api_docs.py from its EXTENSIONS, the handlers in src/ and "
           "docs/api/shapes/: edit those, then run it. -->", "", what, ""]
    if extra:
        out += [extra]
    for call, note in calls:
        out += [f"## `{call}`", "", note, ""]
        ps = params(call, hs)
        if ps:
            out += ["Parameters: " + ", ".join(f"`{n}`" + ("" if r == "optional" else " (required)") for n, r in ps) + ".", ""]
        sh = shape(call)
        if sh == {}:
            out += ["Answers \"ok\" with no fields.", ""]
        elif sh:
            out += ["| Field | Type |", "|---|---|"]
            for path, types in sh.items():
                out.append(f"| `{path}` | {' or '.join(types)} |")
            out.append("")
        elif call not in ("djAudio", "importUpload"):
            out += ["(No answer recorded yet.)", ""]
    out += ["From 1.0, a field here may gain company but never goes away or changes type (see "
            "[STABILITY.md](../STABILITY.md)).", ""]
    return "\n".join(out)


def main():
    hs = handlers()
    stale = []
    for ext in EXTENSIONS:
        path = os.path.join(ROOT, "docs", "api", ext + ".md")
        text = page(ext, hs)
        old = open(path, encoding="utf-8").read() if os.path.exists(path) else None
        if old != text:
            if "--check" in sys.argv:
                stale.append(os.path.relpath(path, ROOT))
            else:
                open(path, "w", encoding="utf-8").write(text)
    if stale:
        sys.exit("out of date (run python3 tools/api_docs.py): " + ", ".join(stale))


if __name__ == "__main__":
    main()
