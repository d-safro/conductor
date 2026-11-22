# Upgrade fixtures

A database made by each older release, filled through its own API by `tools/upgrade_fixture.py`, with its password
key and what it answered (`expected.json`). `tests/upgrade.rs` opens each with the current code and checks every answer
is unchanged. All made up: a test user, a test password, sine-wave songs.

To add one for a new release, build it from its tag and run, for example:

    git worktree add ../conductor-v0.4.0 v0.4.0 && (cd ../conductor-v0.4.0 && cargo build --release)
    python3 tools/upgrade_fixture.py ../conductor-v0.4.0/target/release/conductor CONDUCTOR v0.4.0

Then fold its journal into the database (`PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE; VACUUM;`)
so it's one file.
