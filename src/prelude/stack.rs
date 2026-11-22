//! Prelude's services folder: everything it sets up for you, as one Docker Compose project beside config.toml
//! (`services/`; an earlier version's `downloads-vpn/` keeps being used). One file per part, all started together:
//!
//!   compose.yml      the project: `include:` the parts below that are there
//!   downloads.yml    slskd, alone or inside a VPN (gluetun.rs writes it)
//!   lidarr.yml       Lidarr (its key in lidarr.env)
//!   audiomuse.yml    AudioMuse-AI: its database, web app and worker (keys and logins in audiomuse.env)
//!   pitch.yml        Pitch, "What's playing?" (built here from pitch/)
//!   metronome.yml    Metronome, Automix analysis (built here from metronome/)
//!   ai.yml           Ollama and Interlude's voice (built here from voice/), only when AI is chosen
//!
//! Everything listens on 127.0.0.1 only, on ports chosen so they don't clash. Secrets go in files only you can read.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};

pub const PROJECT: &str = "conductor-services";
/// in the order they're included
pub const PARTS: &[&str] = &["downloads", "lidarr", "audiomuse", "pitch", "metronome", "ai"];

pub const PITCH_FILES: &[(&str, &str)] = &[
    ("Dockerfile", include_str!("../../helpers/pitch/Dockerfile")),
    ("song_id.py", include_str!("../../helpers/pitch/song_id.py")),
];
pub const METRONOME_FILES: &[(&str, &str)] = &[
    ("Dockerfile", include_str!("../../helpers/metronome/Dockerfile")),
    ("mix_analyzer.py", include_str!("../../helpers/metronome/mix_analyzer.py")),
];
pub const VOICE_FILES: &[(&str, &str)] = &[
    ("Dockerfile", include_str!("../../helpers/voice/Dockerfile")),
    ("dj_voice.py", include_str!("../../helpers/voice/dj_voice.py")),
];

/// The folder, beside config.toml.
pub fn dir(config: &Path) -> PathBuf {
    let base = config.parent().unwrap_or(Path::new(".")).to_path_buf();
    let old = base.join("downloads-vpn");
    if old.join("compose.yml").is_file() { old } else { base.join("services") }
}

/// An earlier version's folder held only the downloader, in compose.yml: it becomes downloads.yml.
pub fn migrate(dir: &Path) -> Result<()> {
    let main = dir.join("compose.yml");
    let Ok(text) = std::fs::read_to_string(&main) else { return Ok(()) };
    if text.lines().any(|l| l.trim_start().starts_with("include:")) {
        return Ok(());
    }
    let part: String = text.lines().filter(|l| !l.starts_with("name:")).map(|l| format!("{l}\n")).collect();
    std::fs::write(dir.join("downloads.yml"), part)?;
    refresh(dir)
}

pub fn has_part(dir: &Path, part: &str) -> bool {
    dir.join(format!("{part}.yml")).is_file()
}

/// Write one part (and the files it builds from), then the project file.
pub fn write_part(dir: &Path, part: &str, yaml: &str, files: &[(&str, &str)]) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    migrate(dir)?;
    std::fs::write(dir.join(format!("{part}.yml")), yaml)?;
    if !files.is_empty() {
        let sub = dir.join(part_dir(part));
        std::fs::create_dir_all(&sub)?;
        for (name, text) in files {
            std::fs::write(sub.join(name), text)?;
        }
    }
    refresh(dir)
}

/// A part's own folder (its build files, its data).
pub fn part_dir(part: &str) -> &str {
    if part == "ai" { "voice" } else { part }
}

/// Take a part out (its data folders stay).
pub fn remove_part(dir: &Path, part: &str) -> Result<()> {
    let f = dir.join(format!("{part}.yml"));
    if f.is_file() {
        std::fs::remove_file(f)?;
        refresh(dir)?;
    }
    Ok(())
}

/// compose.yml: the parts that are there.
pub fn refresh(dir: &Path) -> Result<()> {
    let present: Vec<&str> = PARTS.iter().copied().filter(|p| has_part(dir, p)).collect();
    let mut text = format!(
        "# Made by `conductor setup` (Prelude): the services it set up for Conductor, started together.
# Start or update them: docker compose up -d --build   Stop: docker compose down   (in this folder)
name: {PROJECT}
"
    );
    if present.is_empty() {
        text += "services: {}\n";
    } else {
        text += "include:\n";
        for p in present {
            text += &format!("  - {p}.yml\n");
        }
    }
    std::fs::write(dir.join("compose.yml"), text).with_context(|| format!("writing {}", dir.display()))
}

fn private(path: &Path, text: &str) -> Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    crate::config::write_private(path, text.as_bytes()).with_context(|| format!("writing {}", path.display()))
}

/// The port on this computer a part's file gives a container's port (`127.0.0.1:HOST:INNER`). Run again, a part
/// keeps its port: its own container holds it, so it looks taken, and the settings would point at nothing.
pub fn part_port(dir: &Path, part: &str, inner: u16) -> Option<u16> {
    let text = std::fs::read_to_string(dir.join(format!("{part}.yml"))).ok()?;
    text.lines().find_map(|l| {
        let l = l.split('#').next()?.trim().strip_prefix('-')?.trim().trim_matches('"');
        let mut it = l.rsplit(':');
        let i: u16 = it.next()?.parse().ok()?;
        let h: u16 = it.next()?.parse().ok()?;
        (i == inner).then_some(h)
    })
}

/// A value from an env file (KEY=VALUE). Run again, the keys a service already holds are kept.
pub fn env_value(path: &Path, key: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()?.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('=').map(|v| v.trim().to_string())).filter(|v| !v.is_empty())
}

/// SELinux (Fedora, RHEL…) keeps containers out of folders that aren't labelled for them: with it, every folder
/// mounted gets `:z` (Docker labels it once, shared). Elsewhere nothing is added.
pub fn selinux() -> bool {
    std::fs::read_to_string("/sys/fs/selinux/enforce").is_ok()
}

/// A mount's options: "" / ":z", or for read-only ":ro" / ":ro,z".
pub fn opts(read_only: bool) -> &'static str {
    match (read_only, selinux()) {
        (false, false) => "",
        (false, true) => ":z",
        (true, false) => ":ro",
        (true, true) => ":ro,z",
    }
}

pub fn q(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

pub fn ids() -> (u32, u32) {
    #[cfg(unix)]
    {
        let n = |a: &str| Command::new("id").arg(a).output().ok().and_then(|o| String::from_utf8(o.stdout).ok()).and_then(|s| s.trim().parse().ok());
        (n("-u").unwrap_or(1000), n("-g").unwrap_or(1000))
    }
    #[cfg(not(unix))]
    {
        (1000, 1000)
    }
}

// ── the parts ──

pub struct Lidarr<'a> {
    pub port: u16,
    pub key: &'a str,
    /// the music folder and the downloads folder, mounted at the same places as here
    pub music: &'a str,
    pub downloads: Option<&'a str>,
}

pub fn lidarr(dir: &Path, l: &Lidarr) -> Result<()> {
    let (uid, gid) = ids();
    private(&dir.join("lidarr.env"), &format!("# Lidarr's API key (Conductor has the same one). Keep it to yourself.\nLIDARR__AUTH__APIKEY={}\n", l.key))?;
    let z = opts(false);
    let mut vols = format!("      - ./lidarr:/config{z}\n      - {}\n", q(&format!("{0}:{0}{z}", l.music)));
    if let Some(d) = l.downloads.filter(|d| !d.is_empty() && *d != l.music) {
        vols += &format!("      - {}\n", q(&format!("{d}:{d}{z}")));
    }
    let yaml = format!(
        "# Lidarr: albums you want, which Conductor fetches from Soulseek (Stagehand) and files into your music.
services:
  lidarr:
    image: lscr.io/linuxserver/lidarr:latest
    container_name: conductor-lidarr
    env_file: [lidarr.env]
    environment:
      PUID: \"{uid}\"
      PGID: \"{gid}\"
      LIDARR__AUTH__REQUIRED: DisabledForLocalAddresses
    volumes:
{vols}    ports:
      - \"127.0.0.1:{port}:8686\"
    restart: unless-stopped
",
        port = l.port,
    );
    write_part(dir, "lidarr", &yaml, &[])
}

pub struct AudioMuse<'a> {
    pub port: u16,
    /// Conductor as the containers reach it
    pub conductor_url: &'a str,
    /// the Conductor account AudioMuse reads the library with (made for it)
    pub user: &'a str,
    pub password: &'a str,
    /// Conductor's key for AudioMuse's API
    pub api_token: &'a str,
    /// AudioMuse's own web page login
    pub web_password: &'a str,
}

pub fn audiomuse(dir: &Path, a: &AudioMuse) -> Result<()> {
    // its database keeps the password it was made with: an earlier run's stays
    let env = dir.join("audiomuse.env");
    let db_pass = env_value(&env, "POSTGRES_PASSWORD").unwrap_or_else(super::new_key);
    let jwt = env_value(&env, "JWT_SECRET").unwrap_or_else(super::new_key);
    private(
        &dir.join("audiomuse.env"),
        &format!(
            "# AudioMuse-AI's keys and logins. Keep this file to yourself.
# Its web page: user admin, the password below.
POSTGRES_USER=audiomuse
POSTGRES_PASSWORD={db_pass}
POSTGRES_DB=audiomusedb
POSTGRES_HOST=audiomuse-db
POSTGRES_PORT=5432
MEDIASERVER_TYPE=navidrome
NAVIDROME_URL={url}
NAVIDROME_USER={user}
NAVIDROME_PASSWORD={pass}
AUTH_ENABLED=true
AUDIOMUSE_USER=admin
AUDIOMUSE_PASSWORD={web}
API_TOKEN={token}
JWT_SECRET={jwt}
TEMP_DIR=/app/temp_audio
",
            url = a.conductor_url,
            user = a.user,
            pass = a.password,
            web = a.web_password,
            token = a.api_token,
        ),
    )?;
    let yaml = format!(
        "# AudioMuse-AI: listens to your songs once and finds the ones that sound alike (Conductor asks it).
services:
  audiomuse-db:
    image: postgres:15-alpine
    container_name: conductor-audiomuse-db
    env_file: [audiomuse.env]
    volumes:
      - ./audiomuse/db:/var/lib/postgresql/data{z}
    restart: unless-stopped
  audiomuse:
    image: ghcr.io/neptunehub/audiomuse-ai:latest
    container_name: conductor-audiomuse
    depends_on: [audiomuse-db]
    env_file: [audiomuse.env]
    environment:
      SERVICE_TYPE: flask
    extra_hosts: [\"host.docker.internal:host-gateway\"]
    volumes:
      - ./audiomuse/plugins:/app/plugin/installed{z}
    ports:
      - \"127.0.0.1:{port}:8000\"
    restart: unless-stopped
  audiomuse-worker:
    image: ghcr.io/neptunehub/audiomuse-ai:latest
    container_name: conductor-audiomuse-worker
    depends_on: [audiomuse-db]
    env_file: [audiomuse.env]
    environment:
      SERVICE_TYPE: worker
    extra_hosts: [\"host.docker.internal:host-gateway\"]
    volumes:
      - ./audiomuse/plugins-worker:/app/plugin/installed{z}
    restart: unless-stopped
",
        port = a.port,
        z = opts(false),
    );
    write_part(dir, "audiomuse", &yaml, &[])
}

/// The music folders as Pitch and Metronome see them (read-only): one is their /music; several are /music/lib<n>,
/// in the settings' order (Conductor maps paths the same way: api::bridges::helper_path).
fn music_mounts(music: &[String]) -> String {
    let ro = opts(true);
    match music {
        [one] => format!("      - {}\n", q(&format!("{one}:/music{ro}"))),
        many => many.iter().enumerate().map(|(n, m)| format!("      - {}\n", q(&format!("{m}:/music/lib{n}{ro}")))).collect(),
    }
}

/// Pitch: the music folders, read-only; its fingerprints in pitch/data.
pub fn pitch(dir: &Path, port: u16, music: &[String]) -> Result<()> {
    let (uid, gid) = ids();
    let yaml = format!(
        "# Pitch, \"What's playing?\": fingerprints your songs once (slowly, newest first) and recognises short
# recordings against them. Offline: nothing is sent anywhere, and it only knows your own songs.
services:
  pitch:
    build: ./pitch
    image: conductor-pitch:local
    container_name: conductor-pitch
    user: \"{uid}:{gid}\"
    environment:
      MUSIC: /music
      OUT: /out
    volumes:
{music}      - ./pitch/data:/out{z}
    ports:
      - \"127.0.0.1:{port}:4540\"
    cpus: 1
    mem_limit: 768m
    restart: unless-stopped
",
        music = music_mounts(music),
        z = opts(false),
    );
    std::fs::create_dir_all(dir.join("pitch/data"))?;
    write_part(dir, "pitch", &yaml, PITCH_FILES)
}

/// Metronome: the music folders, read-only; its results in `out` (Conductor's mixinfo_dir).
pub fn metronome(dir: &Path, music: &[String], out: &str) -> Result<()> {
    let (uid, gid) = ids();
    std::fs::create_dir_all(out).with_context(|| format!("making {out}"))?;
    let yaml = format!(
        "# Metronome, Automix's analysis: where each song's sound starts and ends, its intro and outro, its tempo.
# No network at all; the songs coming up in your queue go first, then slowly the rest.
services:
  metronome:
    build: ./metronome
    image: conductor-metronome:local
    container_name: conductor-metronome
    user: \"{uid}:{gid}\"
    network_mode: none
    environment:
      MUSIC: /music
      OUT: /out
    volumes:
{music}      - {out}
    cpus: 1
    mem_limit: 512m
    restart: unless-stopped
",
        music = music_mounts(music),
        out = q(&format!("{out}:/out{}", opts(false))),
    );
    write_part(dir, "metronome", &yaml, METRONOME_FILES)
}

/// AI: Ollama (unless one is already running: `ollama_port` None) and Interlude's voice.
pub fn ai(dir: &Path, ollama_port: Option<u16>, voice_port: u16) -> Result<()> {
    let (uid, gid) = ids();
    let mut yaml = String::from(
        "# AI, only because you chose it: Ollama (local language models: Libretto's translations, Interlude's lines)
# and Interlude's voice (Piper, offline text-to-speech). Nothing leaves this computer.
services:
",
    );
    let z = opts(false);
    if let Some(p) = ollama_port {
        yaml += &format!(
            "  ollama:
    image: ollama/ollama:latest
    container_name: conductor-ollama
    environment:
      OLLAMA_KEEP_ALIVE: 10m
    volumes:
      - ./ollama:/root/.ollama{z}
    ports:
      - \"127.0.0.1:{p}:11434\"
    restart: unless-stopped
"
        );
    }
    yaml += &format!(
        "  voice:
    build: ./voice
    image: conductor-voice:local
    container_name: conductor-voice
    user: \"{uid}:{gid}\"
    environment:
      HOME: /tmp
    volumes:
      - ./voice/voices:/voices{z}
    ports:
      - \"127.0.0.1:{voice_port}:4541\"
    mem_limit: 512m
    restart: unless-stopped
"
    );
    std::fs::create_dir_all(dir.join("voice/voices"))?;
    write_part(dir, "ai", &yaml, VOICE_FILES)
}

// ── running them ──

fn docker(dir: &Path, args: &[&str]) -> Result<String> {
    let o = Command::new("docker").arg("compose").args(args).current_dir(dir).output().context("running docker compose (is Docker installed?)")?;
    if !o.status.success() {
        bail!("docker compose {}: {}", args.join(" "), String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or(""));
    }
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}

/// `docker compose config`: the files are valid (nothing is started).
pub fn validate(dir: &Path) -> Result<()> {
    docker(dir, &["config", "-q"]).map(|_| ())
}

/// Start (or update) everything in the folder. An earlier version's downloader project is stopped first: its
/// containers have the same names and now belong to this one. A part taken out (or gluetun, when the downloader
/// leaves the VPN) has its container removed.
pub fn up(dir: &Path) -> Result<()> {
    let old = Command::new("docker").args(["compose", "ls", "-a", "-q"]).output();
    if old.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).lines().any(|l| l.trim() == "conductor-downloads")) {
        let _ = Command::new("docker").args(["compose", "-p", "conductor-downloads", "down"]).output();
    }
    docker(dir, &["up", "-d", "--build", "--remove-orphans"]).map(|_| ())
}

/// A container's command, for after it's up (e.g. Ollama fetching a model).
pub fn exec(container: &str, args: &[&str]) -> Result<()> {
    let s = Command::new("docker").arg("exec").arg(container).args(args).status().context("docker exec")?;
    if !s.success() {
        bail!("{} failed in {container}", args.join(" "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn music_folders_for_the_helpers() {
        let one = super::music_mounts(&["/srv/music".into()]);
        assert!(one.contains("/srv/music:/music:ro") && !one.contains("lib0"), "{one}");
        let two = super::music_mounts(&["/srv/a".into(), "/srv/b".into()]);
        assert!(two.contains("/srv/a:/music/lib0:ro") && two.contains("/srv/b:/music/lib1:ro"), "{two}");
    }
}
