# Helpers

Small services Conductor can use, each in its own container. Prelude (`conductor setup services`) writes them into
its services folder and builds them on your computer; nothing here runs inside Conductor itself.

| Folder | Name | What it does | Conductor's setting |
|---|---|---|---|
| `pitch/` | Pitch | "What's playing?": fingerprints your library once and recognises short recordings, offline | `songid_url` |
| `metronome/` | Metronome | Automix analysis: where each song's sound starts and ends, intro, outro, tempo | `mixinfo_dir` |
| `voice/` | Interlude's voice | Piper text-to-speech for the DJ, offline (only with AI on) | `dj_voice_url` |
