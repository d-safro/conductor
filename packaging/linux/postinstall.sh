#!/bin/sh
# the service's user and its folder (Arch's pacman does this by itself from the files in the package)
if command -v systemd-sysusers >/dev/null 2>&1; then
  systemd-sysusers conductor.conf || true
elif ! id conductor >/dev/null 2>&1; then
  # (no systemd here: the same user, made the classic way)
  useradd --system --home-dir /var/lib/conductor --no-create-home --shell /usr/sbin/nologin --comment "Conductor music server" conductor 2>/dev/null \
    || adduser -S -h /var/lib/conductor -s /sbin/nologin conductor 2>/dev/null || true
fi
if command -v systemd-tmpfiles >/dev/null 2>&1; then
  systemd-tmpfiles --create conductor.conf || true
else
  mkdir -p /var/lib/conductor && chown conductor:conductor /var/lib/conductor 2>/dev/null && chmod 750 /var/lib/conductor || true
fi
if command -v systemctl >/dev/null 2>&1; then
  systemctl daemon-reload >/dev/null 2>&1 || true
  systemctl try-restart conductor.service >/dev/null 2>&1 || true   # an upgrade: the running service, new version
fi
cat <<'MSG'
Conductor is installed. Set it up:
  conductor setup                     (as you: Prelude asks about everything, and can run it as a service for you)
or as a system service running as the "conductor" user (give it read access to your music folders):
  sudo -u conductor conductor --config /var/lib/conductor/config.toml setup
  sudo systemctl enable --now conductor
The guide: /usr/share/doc/conductor/GUIDE.md
MSG
