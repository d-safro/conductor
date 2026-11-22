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

/// A compose file for running Conductor in Docker, beside config.toml.
pub fn docker(ui: &mut dyn Ui, d: &Draft) -> Result<()> {
    let music = d.cfg.get("libraries").and_then(|l| l.as_array()).and_then(|l| l.first()).and_then(|l| l.get("path")).and_then(|p| p.as_str()).unwrap_or("/path/to/music").to_string();
    let data = d.path.parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
    let port = d.str("listen").and_then(|l| l.rsplit(':').next().map(String::from)).unwrap_or_else(|| "4533".into());
    let text = format!(
        "# Conductor in Docker, made by `conductor setup`. Build the image from the source: docker build -f deploy/Dockerfile -t conductor:dev .
services:
  conductor:
    image: conductor:dev
    container_name: conductor
    restart: unless-stopped
    environment:
      CONDUCTOR_DATA_DIR: /data
      CONDUCTOR_MUSIC: /music
    volumes:
      - {data}:/data
      - {music}:/music
    ports:
      - \"{port}:4533\"
"
    );
    let path = d.path.with_file_name("conductor-compose.yml");
    std::fs::write(&path, text)?;
    ui.say(&format!("  ✓ {} (start: docker compose -f {} up -d)", path.display(), path.display()));
    Ok(())
}
