#!/bin/sh
# After the package's files are gone (removed, or an upgrade's old ones replaced): systemd forgets the unit file that
# was taken away, so `systemctl` doesn't go on listing a conductor.service that isn't there. Only where systemd runs.
# The service's user and its data in /var/lib/conductor stay (docs/GUIDE.md says how to remove them by hand).
if [ -d /run/systemd/system ] && command -v systemctl >/dev/null 2>&1; then
  systemctl daemon-reload >/dev/null 2>&1 || true
fi
true
