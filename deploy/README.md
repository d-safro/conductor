# Running Conductor

## Docker (Linux, x86-64 and ARM64)

The published image, `ghcr.io/d-safro/conductor` (tags: the version, its `x.y`, and `latest`):

```bash
docker run -d --name conductor -p 127.0.0.1:4533:4533 \
  -e CONDUCTOR_ADMIN_USER=me -e CONDUCTOR_ADMIN_PASSWORD='…' \
  -v /srv/conductor:/data -v /path/to/music:/music ghcr.io/d-safro/conductor
```

Without the two admin settings, the first admin is made at `/setup` (from the server itself, or with the one-time
link `docker exec conductor conductor setup link` shows). To build the image from the source instead:
`docker build -f deploy/Dockerfile -t conductor:dev .`

The image is small (Alpine + ffmpeg + one static binary) and has a health check: `conductor health`, which asks
`/health` at the address Conductor listens on (`CONDUCTOR_LISTEN`, else `listen` in config.toml; exit 0 or 1). Keep `/data` on a disk
that doesn't go away (the database, the password key, caches and the daily backups live there). The music folder
needs to be writable only if Conductor should file downloads, keep trial songs or save covers
(`CONDUCTOR_READ_ONLY=1` otherwise).

`deploy/compose.example.yml` is a starting point for Docker Compose. Prelude's "With Docker" choice writes a compose
file that reads the same `config.toml` as running it directly (same folders, this computer's network), so the
downloader it sets up works there too.

## Windows

`conductor.exe` is cross-built with MinGW in a container, so nothing needs installing on the build machine:

```bash
docker build -f deploy/Dockerfile.windows --output type=local,dest=dist/windows .
```

On the Windows PC: put `conductor.exe` and `ffmpeg.exe` (from ffmpeg.org builds) in one folder, write a `config.toml`
next to them (see `config.example.toml`; paths like `D:\\Music`), and run `conductor.exe --config config.toml`.
Data goes to `%ProgramData%\conductor` unless `data_dir` says otherwise. If slskd / Lidarr run in Docker Desktop, set
`slskd_downloads` to the folder as Windows sees it and `lidarr_downloads` to the folder as Lidarr sees it.
**As a service** (starts with Windows and again if it fails, log in `<data_dir>\conductor.log`): from the release zip, in PowerShell as
administrator, `powershell -ExecutionPolicy Bypass -File install.ps1 -Music "D:\Music"` (add `-OpenFirewall` for the
local network). Or by hand: `conductor.exe --config C:\ProgramData\conductor\config.toml service install`, then
`service start` / `service stop` / `service uninstall`. `uninstall.ps1` removes it (data kept unless `-RemoveData`).
Release files and the image are built by `.github/workflows/release.yml` on a `v*` tag, after the tests pass on Windows,
Linux x86-64 and Linux ARM64.
