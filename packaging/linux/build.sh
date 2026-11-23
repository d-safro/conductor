#!/bin/sh
# Conductor's Linux packages (.deb, .rpm, Arch) for one architecture, from the program already placed at
# packaging/linux/build/conductor. Used by CI and by the release.
#   packaging/linux/build.sh VERSION ARCH OUTDIR      (ARCH: amd64 or arm64; NFPM=path/to/nfpm if not on PATH)
# A prerelease (1.0.0-rc.1) has to sort before its release on every system, so 1.0.0 replaces it:
#   .deb and .rpm: nfpm writes 1.0.0~rc.1;
#   Arch: nfpm would leave the prerelease out (1.0.0, the same as the release), so it gets pacman's 1.0.0rc.1.
# GitHub renames "~" in a release's file names to "."; the files get that name here, so SHA256SUMS matches.
set -eu
v=$1 a=$2 out=$3
nfpm=${NFPM:-nfpm}
here=$(dirname "$0")
mkdir -p "$out"
for p in deb rpm; do
  VERSION="$v" ARCH="$a" "$nfpm" package -p "$p" -f "$here/nfpm.yaml" -t "$out/"
done
sed 's/^version: .*/&\nversion_schema: none/' "$here/nfpm.yaml" > "$here/build/nfpm-arch.yaml"
VERSION="$(printf %s "$v" | tr -d -)" ARCH="$a" "$nfpm" package -p archlinux -f "$here/build/nfpm-arch.yaml" -t "$out/"
for f in "$out"/*~*; do
  [ -e "$f" ] && mv "$f" "$(printf %s "$f" | tr '~' .)"
done
true
