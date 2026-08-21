use std::{
    collections::HashSet,
    path::Path,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

pub(crate) struct LibraryDb {
    conn: Mutex<Connection>,
}

pub(crate) struct FileFingerprint {
    pub(crate) hash: String,
    pub(crate) size_bytes: u64,
}

impl LibraryDb {
    pub(crate) fn open(path: &Path) -> anyhow::Result<Self> {
        if path != Path::new(":memory:")
            && let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("couldn't create dir {}", parent.display()))?;
        }

        let conn = Connection::open(path)
            .with_context(|| format!("couldn't open library db at {}", path.display()))?;

        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        let _ = conn.pragma_update(None, "synchronous", "NORMAL");

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS tracks (
                guid            TEXT PRIMARY KEY,
                root            INTEGER NOT NULL,
                relative_path   TEXT NOT NULL,
                fingerprint     TEXT NOT NULL,
                size_bytes      INTEGER NOT NULL,
                added_at        INTEGER NOT NULL,
                last_seen       INTEGER NOT NULL,
                UNIQUE(root, relative_path)
            );
            CREATE INDEX IF NOT EXISTS idx_tracks_fingerprint ON tracks(fingerprint);
            ",
        )
        .context("couldn't set up library db schema")?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub(crate) fn resolve_guid(
        &self,
        root: usize,
        relative_path: &str,
        fingerprint: &FileFingerprint,
    ) -> anyhow::Result<String> {
        let now = now_unix();
        let conn = self.conn.lock().expect("library db mutex poisoned");
        let root = root as i64;

        if let Some((guid, old_root, old_path)) = conn
            .query_row(
                "SELECT guid, root, relative_path FROM tracks WHERE fingerprint = ?1",
                params![fingerprint.hash],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .context("library db lookup by fingerprint failed")?
        {
            if old_root != root || old_path != relative_path {
                tracing::info!(
                    "Detected moved/renamed file: '{old_path}' -> '{relative_path}' (root {old_root} -> {root}), keeping id {guid}"
                );
                conn.execute(
                    "UPDATE tracks SET root = ?1, relative_path = ?2, size_bytes = ?3, last_seen = ?4 WHERE guid = ?5",
                    params![root, relative_path, fingerprint.size_bytes as i64, now, guid],
                )
                .context("library db update (move) failed")?;
            } else {
                conn.execute(
                    "UPDATE tracks SET size_bytes = ?1, last_seen = ?2 WHERE guid = ?3",
                    params![fingerprint.size_bytes as i64, now, guid],
                )
                .context("library db touch failed")?;
            }
            return Ok(guid);
        }

        if let Some(guid) = conn
            .query_row(
                "SELECT guid FROM tracks WHERE root = ?1 AND relative_path = ?2",
                params![root, relative_path],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .context("library db lookup by path failed")?
        {
            conn.execute(
                "UPDATE tracks SET fingerprint = ?1, size_bytes = ?2, last_seen = ?3 WHERE guid = ?4",
                params![fingerprint.hash, fingerprint.size_bytes as i64, now, guid],
            )
            .context("library db update (retag) failed")?;
            return Ok(guid);
        }

        let guid = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO tracks (guid, root, relative_path, fingerprint, size_bytes, added_at, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![guid, root, relative_path, fingerprint.hash, fingerprint.size_bytes as i64, now],
        )
        .context("library db insert failed")?;
        Ok(guid)
    }

    pub(crate) fn assign_new_guid(
        &self,
        root: usize,
        relative_path: &str,
        fingerprint: &FileFingerprint,
    ) -> anyhow::Result<String> {
        let now = now_unix();
        let conn = self.conn.lock().expect("library db mutex poisoned");
        let root = root as i64;
        let guid = uuid::Uuid::new_v4().to_string();

        conn.execute(
            "INSERT INTO tracks (guid, root, relative_path, fingerprint, size_bytes, added_at, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
             ON CONFLICT(root, relative_path) DO UPDATE SET
                guid = excluded.guid,
                fingerprint = excluded.fingerprint,
                size_bytes = excluded.size_bytes,
                last_seen = excluded.last_seen",
            params![guid, root, relative_path, fingerprint.hash, fingerprint.size_bytes as i64, now],
        )
        .context("library db insert (fresh guid) failed")?;
        Ok(guid)
    }

    pub(crate) fn prune_missing_for_root(
        &self,
        root: usize,
        seen_guids: &HashSet<String>,
    ) -> anyhow::Result<usize> {
        let conn = self.conn.lock().expect("library db mutex poisoned");
        let mut stmt = conn.prepare("SELECT guid, relative_path FROM tracks WHERE root = ?1")?;
        let stale: Vec<(String, String)> = stmt
            .query_map(params![root as i64], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .filter_map(|r| r.ok())
            .filter(|(guid, _)| !seen_guids.contains(guid))
            .collect();
        drop(stmt);

        for (guid, relative_path) in &stale {
            conn.execute("DELETE FROM tracks WHERE guid = ?1", params![guid])?;
            tracing::info!(
                "Removed deleted file from library db: root {root} '{relative_path}' (id {guid})"
            );
        }
        Ok(stale.len())
    }

    pub(crate) fn remove_path(&self, root: usize, relative_path: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().expect("library db mutex poisoned");
        let affected = conn.execute(
            "DELETE FROM tracks WHERE root = ?1 AND relative_path = ?2",
            params![root as i64, relative_path],
        )?;
        if affected > 0 {
            tracing::info!("Removed deleted file from library db: root {root} '{relative_path}'");
        }
        Ok(())
    }

    pub(crate) fn remove_prefix(&self, root: usize, rel_prefix: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().expect("library db mutex poisoned");
        let like_pattern = format!("{}/%", rel_prefix.replace('%', "\\%").replace('_', "\\_"));
        let affected = conn.execute(
            "DELETE FROM tracks WHERE root = ?1 AND (relative_path = ?2 OR relative_path LIKE ?3 ESCAPE '\\')",
            params![root as i64, rel_prefix, like_pattern],
        )?;
        if affected > 0 {
            tracing::info!(
                "Removed {affected} deleted file(s) from library db under root {root} '{rel_prefix}/'"
            );
        }
        Ok(())
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub(crate) fn fingerprint_file(path: &Path) -> std::io::Result<FileFingerprint> {
    use std::io::{Read, Seek, SeekFrom};
    const SAMPLE_SIZE: u64 = 256 * 1024;

    let mut file = std::fs::File::open(path)?;
    let size_bytes = file.metadata()?.len();

    let mut hasher = Sha256::new();
    hasher.update(size_bytes.to_le_bytes());

    if size_bytes > 0 {
        let sample_len = SAMPLE_SIZE.min(size_bytes);
        let start = (size_bytes / 2).saturating_sub(sample_len / 2);
        file.seek(SeekFrom::Start(start))?;

        let mut buf = vec![0u8; sample_len as usize];
        let mut read_total = 0usize;
        while read_total < buf.len() {
            let n = file.read(&mut buf[read_total..])?;
            if n == 0 {
                break;
            }
            read_total += n;
        }
        hasher.update(&buf[..read_total]);
    }

    let digest = hasher.finalize();
    let hash = digest.iter().map(|b| format!("{b:02x}")).collect();
    Ok(FileFingerprint { hash, size_bytes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn memdb() -> LibraryDb {
        LibraryDb::open(Path::new(":memory:")).expect("open in-memory db")
    }

    fn fp(hash: &str, size_bytes: u64) -> FileFingerprint {
        FileFingerprint {
            hash: hash.to_string(),
            size_bytes,
        }
    }

    #[test]
    fn new_file_gets_a_fresh_guid() {
        let db = memdb();
        let guid = db.resolve_guid(0, "song.mp3", &fp("aaa", 100)).unwrap();
        assert_eq!(guid.len(), 36);
    }

    #[test]
    fn rescanning_the_same_file_keeps_the_same_guid() {
        let db = memdb();
        let first = db.resolve_guid(0, "song.mp3", &fp("aaa", 100)).unwrap();
        let second = db.resolve_guid(0, "song.mp3", &fp("aaa", 100)).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn renamed_file_with_same_content_keeps_its_guid() {
        let db = memdb();
        let original = db.resolve_guid(0, "old_name.mp3", &fp("aaa", 100)).unwrap();

        let renamed = db.resolve_guid(0, "new_name.mp3", &fp("aaa", 100)).unwrap();
        assert_eq!(original, renamed);

        db.remove_path(0, "old_name.mp3").unwrap();
        let renamed_again = db.resolve_guid(0, "new_name.mp3", &fp("aaa", 100)).unwrap();
        assert_eq!(original, renamed_again);
    }

    #[test]
    fn moved_across_roots_keeps_its_guid() {
        let db = memdb();
        let original = db
            .resolve_guid(0, "Music/song.mp3", &fp("bbb", 100))
            .unwrap();
        let moved = db.resolve_guid(1, "song.mp3", &fp("bbb", 100)).unwrap();
        assert_eq!(original, moved);
    }

    #[test]
    fn different_content_at_a_reused_path_keeps_the_paths_guid() {
        let db = memdb();
        let original = db.resolve_guid(0, "song.mp3", &fp("aaa", 100)).unwrap();
        let replaced = db.resolve_guid(0, "song.mp3", &fp("ccc", 250)).unwrap();
        assert_eq!(original, replaced);
    }

    #[test]
    fn distinct_files_get_distinct_guids() {
        let db = memdb();
        let a = db.resolve_guid(0, "a.mp3", &fp("aaa", 100)).unwrap();
        let b = db.resolve_guid(0, "b.mp3", &fp("bbb", 200)).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn prune_missing_for_root_deletes_untouched_rows_only() {
        let db = memdb();
        let kept = db.resolve_guid(0, "keep.mp3", &fp("aaa", 100)).unwrap();
        let gone = db.resolve_guid(0, "gone.mp3", &fp("bbb", 200)).unwrap();

        let mut seen = HashSet::new();
        seen.insert(kept.clone());
        let removed = db.prune_missing_for_root(0, &seen).unwrap();
        assert_eq!(removed, 1);

        let fresh = db.resolve_guid(0, "other.mp3", &fp("ccc", 300)).unwrap();
        assert_ne!(fresh, gone);
    }

    #[test]
    fn prune_missing_ignores_other_roots() {
        let db = memdb();
        let other_root = db.resolve_guid(1, "song.mp3", &fp("aaa", 100)).unwrap();
        let removed = db.prune_missing_for_root(0, &HashSet::new()).unwrap();
        assert_eq!(removed, 0);
        let again = db.resolve_guid(1, "song.mp3", &fp("aaa", 100)).unwrap();
        assert_eq!(other_root, again);
    }

    #[test]
    fn remove_prefix_deletes_directory_contents_but_not_siblings() {
        let db = memdb();
        db.resolve_guid(0, "Album/one.mp3", &fp("a", 1)).unwrap();
        db.resolve_guid(0, "Album/two.mp3", &fp("b", 2)).unwrap();
        db.resolve_guid(0, "Unrelated.mp3", &fp("c", 3)).unwrap();

        db.remove_prefix(0, "Album").unwrap();

        let leftover_count = db.prune_missing_for_root(0, &HashSet::new()).unwrap();
        assert_eq!(leftover_count, 1);
    }

    #[test]
    fn remove_path_is_a_noop_if_the_guid_already_moved_away() {
        let db = memdb();
        let guid = db.resolve_guid(0, "old.mp3", &fp("aaa", 100)).unwrap();
        let moved = db.resolve_guid(0, "new.mp3", &fp("aaa", 100)).unwrap();
        assert_eq!(guid, moved);
        db.remove_path(0, "old.mp3").unwrap();
        let still_there = db.resolve_guid(0, "new.mp3", &fp("aaa", 100)).unwrap();
        assert_eq!(still_there, guid);
    }

    #[test]
    fn fingerprint_is_stable_and_survives_a_copy_to_a_new_path() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        let b = dir.path().join("b.bin");
        let data = vec![7u8; 500_000];
        fs::write(&a, &data).unwrap();
        fs::write(&b, &data).unwrap();

        let fa = fingerprint_file(&a).unwrap();
        let fb = fingerprint_file(&b).unwrap();
        assert_eq!(fa.hash, fb.hash);
        assert_eq!(fa.size_bytes, 500_000);
    }

    #[test]
    fn fingerprint_differs_for_different_content() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        let b = dir.path().join("b.bin");
        fs::write(&a, vec![1u8; 10_000]).unwrap();
        fs::write(&b, vec![2u8; 10_000]).unwrap();

        assert_ne!(
            fingerprint_file(&a).unwrap().hash,
            fingerprint_file(&b).unwrap().hash
        );
    }
}
