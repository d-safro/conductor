#!/bin/sh
# Removed (not upgraded): stop the system service if it runs. Its data in /var/lib/conductor stays.
# ($1: Debian "remove" / "upgrade", RPM 0 / 1+, Arch the old version on removal only)
case "$1" in upgrade | [1-9]*) [ "$1" = upgrade ] || [ "$1" -ge 1 ] 2>/dev/null && exit 0 ;; esac
if command -v systemctl >/dev/null 2>&1; then systemctl disable --now conductor >/dev/null 2>&1 || true; fi
