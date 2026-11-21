-- Albums Lidarr wants, fetched from Soulseek (Soularr's job): one folder from one user, then Lidarr imports it.
CREATE TABLE lidarr_grab (
    album_id     INTEGER PRIMARY KEY,          -- Lidarr's album id
    title        TEXT NOT NULL,
    artist       TEXT NOT NULL,
    status       TEXT NOT NULL DEFAULT 'retry', -- retry · downloading · importing · done · notfound
    slsk_user    TEXT,
    folder       TEXT,                          -- the remote folder
    files_json   TEXT NOT NULL DEFAULT '[]',    -- remote file names being downloaded
    local_dir    TEXT,                          -- "<Artist> - <Album>" in the downloads folder
    command_id   INTEGER,                       -- Lidarr's import command
    searches     INTEGER NOT NULL DEFAULT 0,
    last_search  INTEGER NOT NULL DEFAULT 0,
    tried_json   TEXT NOT NULL DEFAULT '[]',    -- users whose copy failed (download or import): not again
    since        INTEGER NOT NULL DEFAULT 0,
    error        TEXT NOT NULL DEFAULT ''
);
