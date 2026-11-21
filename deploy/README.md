# Running Maestro

## Docker (Linux)

```bash
docker build -f deploy/Dockerfile -t maestro:dev .
docker run -d --name maestro -p 127.0.0.1:4533:4533 \
  -e MAESTRO_ADMIN_USER=me -e MAESTRO_ADMIN_PASSWORD='…' \
  -v /srv/maestro:/data -v /path/to/music:/music maestro:dev
```

The image is small (Alpine + ffmpeg + one static binary) and has a health check on `/health`. Keep `/data` on a disk
that doesn't go away (the database, the password key and caches live there). The music folder needs to be writable
only if Maestro should file downloads, keep trial songs or save covers (`MAESTRO_READ_ONLY=1` otherwise).

`deploy/compose.example.yml` is a starting point for Docker Compose.

## Windows

`maestro.exe` is cross-built with MinGW in a container, so nothing needs installing on the build machine:

```bash
docker build -f deploy/Dockerfile.windows --output type=local,dest=dist/windows .
```

On the Windows PC: put `maestro.exe` and `ffmpeg.exe` (from ffmpeg.org builds) in one folder, write a `config.toml`
next to them (see `config.example.toml`; paths like `D:\\Music`), and run `maestro.exe --config config.toml`.
Data goes to `%ProgramData%\maestro` unless `data_dir` says otherwise. If slskd / Lidarr run in Docker Desktop, set
`slskd_downloads` to the folder as Windows sees it and `lidarr_downloads` to the folder as Lidarr sees it.
A Windows service and an installer are on the roadmap; for now use Task Scheduler ("at log on") or NSSM.
