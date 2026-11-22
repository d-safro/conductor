# What stays the same

From 1.0 on, Conductor follows [semantic versioning](https://semver.org): **1.x.y** with a new **y** only fixes things,
a new **x** adds things, and only a new major version (2.0) may take anything away or change how it works. Before 1.0
(0.x), a new minor version could still change things, and the release notes say so.

## Promised from 1.0

| What | The promise |
|---|---|
| **Your data** | Every release opens the data of every earlier release and brings it up to date by itself, with nothing lost. This is tested on each change with databases made by the earlier releases (`tests/upgrade.rs`). |
| **Settings** | Every name in `config.toml` and `secrets.toml`, and every `CONDUCTOR_*` environment name, keeps its meaning. A renamed setting keeps working under its old name (with a note in the log) until the next major version, as the `MAESTRO_*` names do. |
| **The Subsonic / OpenSubsonic API** | Calls, parameters and fields keep working as they are. New fields and calls may be added (apps ignore what they don't know). |
| **Conductor's own API extensions** | Each `encore.*` extension listed by `getOpenSubsonicExtensions` keeps its calls and fields at the version it lists; a change that would break an app is a new version of it, with the old one kept alongside. |
| **Commands** | `conductor setup` (and its `--yes` flags), `serve`, `scan`, `user add` / `user passwd`, `apikey`, `backup`, `restore`, `import`, `import-navidrome`, and on Windows `service`. |
| **Backups** | A backup made by any 1.x release can be restored by any later one. |
| **Downloads and the image** | The files on each release (`conductor-<version>-<system>`) and the Docker image's tags: `<version>`, `<major>.<minor>`, `latest`. |

## Not promised

These may change in any release: the admin page's own API (`/admin/api/…`, used only by Podium), the web pages'
look and HTML, the Navidrome stand-in beyond what Encore uses, log messages, the layout of the cache folder, and the
files in Prelude's services folder (run `conductor setup services` again after an upgrade to refresh them).

## Going back

A newer release's data can't be opened by an older one. To go back after an upgrade, restore the backup made before
it with the older version (`conductor backup` before upgrading; Conductor also keeps a daily one).

## How a change is retired

1. The new way arrives, and the old one keeps working; the release notes and the log say what to use instead.
2. The old one goes only in the next major version, and the notes for that version list everything that went.
