//! Prelude's last step: running Conductor for good. Each action is confirmed first.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};

use super::{Draft, ui::Ui};

pub fn unit(exe: &Path, config: &Path, user: Option<&str>) -> String {
    format!(
        "# Conductor, set up by `conductor setup`.
[Unit]
Description=Conductor music server
After=network-online.target
Wants=network-online.target

[Service]
ExecStart={} --config {} serve
Restart=on-failure
RestartSec=5
{}
[Install]
WantedBy={}
",
        exe.display(),
        config.display(),
        user.map(|u| format!("User={u}\n")).unwrap_or_default(),
        if user.is_some() { "multi-user.target" } else { "default.target" }
    )
}

fn run(ui: &mut dyn Ui, cmd: &[&str]) -> Result<()> {
    ui.say(&format!("  $ {}", cmd.join(" ")));
    let st = Command::new(cmd[0]).args(&cmd[1..]).status().with_context(|| format!("running {}", cmd[0]))?;
    if !st.success() {
        bail!("{} didn't work", cmd.join(" "));
    }
    Ok(())
}

pub fn systemd_user(ui: &mut dyn Ui, exe: &Path, config: &Path) -> Result<()> {
    let home = std::env::var_os("HOME").map(PathBuf::from).context("no home folder")?;
    let path = home.join(".config/systemd/user/conductor.service");
    if !ui.yes(&format!("Write {}?", path.display()), true) {
        return Ok(());
    }
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(&path, unit(exe, config, None))?;
    ui.say(&format!("  ✓ {}", path.display()));
    if ui.yes("Start it now and whenever you log in (systemctl --user enable --now conductor)?", true) {
        run(ui, &["systemctl", "--user", "daemon-reload"])?;
        run(ui, &["systemctl", "--user", "enable", "--now", "conductor"])?;
    }
    if ui.yes("Keep it running when you're logged out too (loginctl enable-linger)?", true) {
        let user = std::env::var("USER").unwrap_or_default();
        run(ui, &["loginctl", "enable-linger", &user])?;
    }
    Ok(())
}

fn is_root() -> bool {
    Command::new("id").arg("-u").output().is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
}

pub fn systemd_system(ui: &mut dyn Ui, exe: &Path, config: &Path) -> Result<()> {
    let user = std::env::var("SUDO_USER").or_else(|_| std::env::var("USER")).unwrap_or_else(|_| "root".into());
    let text = unit(exe, config, Some(&user));
    if !is_root() {
        let tmp = config.with_file_name("conductor.service");
        std::fs::write(&tmp, &text)?;
        ui.say("  This needs root. Run:");
        ui.say(&format!("    sudo cp {} /etc/systemd/system/conductor.service", tmp.display()));
        ui.say("    sudo systemctl daemon-reload && sudo systemctl enable --now conductor");
        return Ok(());
    }
    let path = Path::new("/etc/systemd/system/conductor.service");
    if !ui.yes(&format!("Write {} (runs as {user})?", path.display()), true) {
        return Ok(());
    }
    std::fs::write(path, text)?;
    if ui.yes("Start it now and at every boot?", true) {
        run(ui, &["systemctl", "daemon-reload"])?;
        run(ui, &["systemctl", "enable", "--now", "conductor"])?;
    }
    Ok(())
}

pub fn windows(ui: &mut dyn Ui, exe: &Path, config: &Path) -> Result<()> {
    if !ui.yes("Install the Conductor service (starts with Windows; needs an administrator window)?", true) {
        return Ok(());
    }
    let exe_s = exe.to_string_lossy().to_string();
    let cfg_s = config.to_string_lossy().to_string();
    run(ui, &[&exe_s, "--config", &cfg_s, "service", "install"])?;
    if ui.yes("Start it now?", true) {
        run(ui, &[&exe_s, "service", "start"])?;
    }
    Ok(())
}

/// The published image (Linux, x86-64 and ARM64).
pub const IMAGE: &str = "ghcr.io/d-safro/conductor:latest";

/// A compose file for running Conductor in Docker, beside config.toml. The container sees the same folders at the
/// same places and shares this computer's network, so config.toml works unchanged in it (its address, the music
/// folders, and the downloader's address).
pub fn docker_yaml(d: &Draft) -> String {
    let mut folders: Vec<String> = d
        .cfg
        .get("libraries")
        .and_then(|l| l.as_array())
        .into_iter()
        .flatten()
        .filter_map(|l| l.get("path").and_then(|p| p.as_str()).map(String::from))
        .collect();
    let data = d.str("data_dir").unwrap_or_else(|| d.path.parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default());
    let cfg_dir = d.path.parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
    for extra in [d.str("slskd_downloads"), d.str("podcast_dir"), Some(cfg_dir.clone())].into_iter().flatten() {
        if !extra.is_empty() && extra != data && !folders.contains(&extra) {
            folders.push(extra);
        }
    }
    let (uid, gid) = ids();
    let q = |s: &str| serde_json::to_string(s).unwrap_or_default();
    let mut vols = format!("      - {}\n", q(&format!("{data}:{data}")));
    for f in &folders {
        vols += &format!("      - {}\n", q(&format!("{f}:{f}")));
    }
    format!(
        "# Conductor in Docker, made by `conductor setup`. Start: docker compose -f conductor-compose.yml up -d
# It reads the same config.toml as Conductor outside Docker: the same folders, mounted at the same places,
# and this computer's network (so its address and the downloader's work as they are).
services:
  conductor:
    image: {IMAGE}
    container_name: conductor
    restart: unless-stopped
    network_mode: host
    user: \"{uid}:{gid}\"
    environment:
      CONDUCTOR_CONFIG: {cfg}
      # the image's own defaults (/data, /music), cleared: config.toml says where things are
      CONDUCTOR_DATA_DIR: \"\"
      CONDUCTOR_MUSIC: \"\"
      CONDUCTOR_LISTEN: \"\"
    volumes:
{vols}",
        cfg = q(&d.path.to_string_lossy()),
    )
}

pub fn docker(ui: &mut dyn Ui, d: &Draft) -> Result<()> {
    let path = d.path.with_file_name("conductor-compose.yml");
    std::fs::write(&path, docker_yaml(d))?;
    ui.say(&format!("  ✓ {} (start: docker compose -f {} up -d)", path.display(), path.display()));
    Ok(())
}

fn ids() -> (u32, u32) {
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
