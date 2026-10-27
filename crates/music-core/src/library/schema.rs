// Ported from ArchiveTune app/src/main/kotlin/moe/rukamori/archivetune/db/entities/*.kt and
// app/schemas/ — © Rukamori and ArchiveTune contributors, GPL-3.0

//! Schema migrations, applied in order and tracked with `PRAGMA user_version`.
//!
//! Each entry upgrades the database from version `index` to `index + 1`. Only ever append.

/// Version a freshly opened database ends up at.
pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

pub(super) const MIGRATIONS: &[&str] = &[
    // v1
    r#"
    CREATE TABLE song (
        id                TEXT PRIMARY KEY NOT NULL,
        title             TEXT NOT NULL,
        artists           TEXT NOT NULL DEFAULT '',
        artist_refs       TEXT NOT NULL DEFAULT '[]',
        album_name        TEXT,
        album_id          TEXT,
        album_artist      TEXT,
        duration_secs     INTEGER,
        thumbnail_url     TEXT,
        explicit          INTEGER NOT NULL DEFAULT 0,
        local_path        TEXT,
        track_number      INTEGER,
        disc_number       INTEGER,
        year              INTEGER,
        rg_track_gain     REAL,
        rg_track_peak     REAL,
        rg_album_gain     REAL,
        rg_album_peak     REAL,
        liked_at          INTEGER,
        in_library_at     INTEGER,
        created_at        INTEGER NOT NULL,
        updated_at        INTEGER NOT NULL,
        total_play_ms     INTEGER NOT NULL DEFAULT 0,
        play_count        INTEGER NOT NULL DEFAULT 0,
        last_played_at    INTEGER,
        downloaded_at     INTEGER,
        download_complete INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX song_album_id ON song(album_id);
    CREATE INDEX song_liked_at ON song(liked_at);
    CREATE INDEX song_in_library_at ON song(in_library_at);

    CREATE TABLE artist (
        id            TEXT PRIMARY KEY NOT NULL,
        name          TEXT NOT NULL,
        thumbnail_url TEXT,
        bookmarked_at INTEGER,
        created_at    INTEGER NOT NULL,
        updated_at    INTEGER NOT NULL
    );

    CREATE TABLE album (
        id            TEXT PRIMARY KEY NOT NULL,
        title         TEXT NOT NULL,
        artists       TEXT NOT NULL DEFAULT '',
        year          INTEGER,
        thumbnail_url TEXT,
        playlist_id   TEXT,
        song_count    INTEGER,
        duration_secs INTEGER,
        explicit      INTEGER NOT NULL DEFAULT 0,
        bookmarked_at INTEGER,
        created_at    INTEGER NOT NULL,
        updated_at    INTEGER NOT NULL
    );

    CREATE TABLE song_artist (
        song_id   TEXT NOT NULL REFERENCES song(id) ON DELETE CASCADE,
        artist_id TEXT NOT NULL REFERENCES artist(id) ON DELETE CASCADE,
        position  INTEGER NOT NULL,
        PRIMARY KEY (song_id, artist_id)
    );
    CREATE INDEX song_artist_artist ON song_artist(artist_id);

    CREATE TABLE song_album (
        song_id     TEXT NOT NULL REFERENCES song(id) ON DELETE CASCADE,
        album_id    TEXT NOT NULL REFERENCES album(id) ON DELETE CASCADE,
        track_index INTEGER,
        PRIMARY KEY (song_id, album_id)
    );
    CREATE INDEX song_album_album ON song_album(album_id);

    CREATE TABLE playlist (
        id                TEXT PRIMARY KEY NOT NULL,
        name              TEXT NOT NULL,
        browse_id         TEXT UNIQUE,
        editable          INTEGER NOT NULL DEFAULT 1,
        thumbnail_url     TEXT,
        remote_song_count INTEGER,
        bookmarked_at     INTEGER,
        created_at        INTEGER NOT NULL,
        updated_at        INTEGER NOT NULL
    );

    CREATE TABLE playlist_entry (
        id           INTEGER PRIMARY KEY AUTOINCREMENT,
        playlist_id  TEXT NOT NULL REFERENCES playlist(id) ON DELETE CASCADE,
        song_id      TEXT NOT NULL REFERENCES song(id) ON DELETE CASCADE,
        position     INTEGER NOT NULL,
        set_video_id TEXT
    );
    CREATE INDEX playlist_entry_order ON playlist_entry(playlist_id, position);
    CREATE INDEX playlist_entry_song ON playlist_entry(song_id);

    CREATE TABLE play_event (
        id        INTEGER PRIMARY KEY AUTOINCREMENT,
        song_id   TEXT NOT NULL REFERENCES song(id) ON DELETE CASCADE,
        played_at INTEGER NOT NULL,
        played_ms INTEGER NOT NULL
    );
    CREATE INDEX play_event_played_at ON play_event(played_at);
    CREATE INDEX play_event_song ON play_event(song_id);

    CREATE TABLE search_history (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        query       TEXT NOT NULL UNIQUE,
        searched_at INTEGER NOT NULL
    );

    CREATE TABLE lyrics (
        song_id    TEXT NOT NULL,
        provider   TEXT NOT NULL,
        text       TEXT NOT NULL,
        fetched_at INTEGER NOT NULL,
        PRIMARY KEY (song_id, provider)
    );

    CREATE TABLE kv (
        key   TEXT PRIMARY KEY NOT NULL,
        value TEXT NOT NULL
    );

    CREATE TABLE local_file (
        path       TEXT PRIMARY KEY NOT NULL,
        song_id    TEXT NOT NULL REFERENCES song(id) ON DELETE CASCADE,
        mtime_ms   INTEGER NOT NULL,
        size       INTEGER NOT NULL,
        scanned_at INTEGER NOT NULL
    );
    CREATE INDEX local_file_song ON local_file(song_id);
    "#,
];
