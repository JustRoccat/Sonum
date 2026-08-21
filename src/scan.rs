use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use lofty::{file::AudioFile, prelude::*, probe::Probe, tag::ItemKey};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

use crate::state::{AppState, LibraryEvent, ScanMeta, Track};
use crate::util::short_hash;

const AUDIO_EXTENSIONS: &[&str] = &["mp3", "flac", "ogg", "opus", "m4a", "wav", "aac"];

pub(crate) fn timed_scan(music_dirs: &[PathBuf]) -> (HashMap<String, Track>, Duration) {
    let started = Instant::now();
    let tracks = scan_all_libraries(music_dirs);
    (tracks, started.elapsed())
}

fn preserve_added_at(new_tracks: &mut HashMap<String, Track>, old_tracks: &HashMap<String, Track>) {
    for (id, track) in new_tracks.iter_mut() {
        if let Some(old) = old_tracks.get(id) {
            track.added_at = track.added_at.min(old.added_at);
        }
    }
}

pub(crate) async fn perform_scan(state: &Arc<AppState>) -> usize {
    let music_dirs = state.music_dirs.clone();
    let old_tracks = state.tracks.read().await.clone();

    let (mut new_tracks, duration) = tokio::task::spawn_blocking(move || timed_scan(&music_dirs))
        .await
        .unwrap_or_else(|e| {
            tracing::error!("background scan blew up: {e}");
            (HashMap::new(), Duration::default())
        });
    preserve_added_at(&mut new_tracks, &old_tracks);

    let count = new_tracks.len();
    *state.tracks.write().await = new_tracks;
    *state.scan_meta.write().await = ScanMeta {
        last_scan: SystemTime::now(),
        duration_ms: duration.as_millis() as u64,
    };

    state.lyrics_cache.write().await.clear();
    state.art_cache.write().await.clear();
    state.emit(LibraryEvent::Rescanned {
        tracks_total: count,
        duration_ms: duration.as_millis() as u64,
    });
    count
}

// finds which configured root a filesystem path lives under, if any.
fn find_root_for_path(music_dirs: &[PathBuf], path: &Path) -> Option<usize> {
    music_dirs.iter().position(|dir| path.starts_with(dir))
}

pub(crate) async fn perform_incremental_scan(
    state: &Arc<AppState>,
    changed_paths: Vec<PathBuf>,
) -> usize {
    let music_dirs = state.music_dirs.clone();
    let snapshot = state.tracks.read().await.clone();

    let (updated_tracks, duration) = tokio::task::spawn_blocking(move || {
        let started = Instant::now();
        let mut tracks = snapshot;
        for path in changed_paths {
            if let Some(root) = find_root_for_path(&music_dirs, &path) {
                apply_path_change(root, &music_dirs[root], &path, &mut tracks);
            }
        }
        (tracks, started.elapsed())
    })
    .await
    .unwrap_or_else(|e| {
        tracing::error!("incremental scan task blew up: {e}");
        (HashMap::new(), Duration::default())
    });

    let count = updated_tracks.len();
    *state.tracks.write().await = updated_tracks;
    *state.scan_meta.write().await = ScanMeta {
        last_scan: SystemTime::now(),
        duration_ms: duration.as_millis() as u64,
    };

    state.lyrics_cache.write().await.clear();
    state.art_cache.write().await.clear();
    state.emit(LibraryEvent::Rescanned {
        tracks_total: count,
        duration_ms: duration.as_millis() as u64,
    });

    count
}

fn apply_path_change(
    root: usize,
    root_dir: &Path,
    changed_path: &Path,
    tracks: &mut HashMap<String, Track>,
) {
    let Ok(rel) = changed_path.strip_prefix(root_dir) else {
        // event outside this root (shouldnt happen, but be defensive)
        return;
    };
    let rel_path = rel.to_string_lossy().replace('\\', "/");
    if rel_path.is_empty() {
        return;
    }

    if changed_path.is_dir() {
        remove_prefixed(tracks, root, &rel_path);
        for entry in walkdir::WalkDir::new(changed_path)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            if let Some(track) = try_read_one(root, root_dir, entry.path()) {
                tracks.insert(track.id.clone(), track);
            }
        }
        return;
    }

    if changed_path.is_file() {
        if let Some(track) = try_read_one(root, root_dir, changed_path) {
            tracks.insert(track.id.clone(), track);
        }
        return;
    }

    tracks.remove(&track_id(root, &rel_path));
    remove_prefixed(tracks, root, &rel_path);
}

fn remove_prefixed(tracks: &mut HashMap<String, Track>, root: usize, rel_path: &str) {
    let prefix = format!("{rel_path}/");
    tracks.retain(|_, t| {
        t.root != root || (t.relative_path != rel_path && !t.relative_path.starts_with(&prefix))
    });
}

fn try_read_one(root: usize, root_dir: &Path, path: &Path) -> Option<Track> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();
    if !is_audio_extension(&ext) {
        return None;
    }
    let relative_path = match path.strip_prefix(root_dir) {
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => return None,
    };
    match read_track_metadata(root, path, &relative_path, &ext) {
        Ok(track) => Some(track),
        Err(e) => {
            tracing::warn!("Skipped '{}': {e}", path.display());
            None
        }
    }
}

pub(crate) fn spawn_music_dir_watcher(state: Arc<AppState>) -> anyhow::Result<RecommendedWatcher> {
    let (tx, rx) = std::sync::mpsc::channel::<notify::Result<Event>>();

    let mut watcher = notify::recommended_watcher(move |res| {
        // notify callback runs on its own thread, just forward the event
        let _ = tx.send(res);
    })
    .context("couldn't set up the file watcher (notify)")?;

    for dir in &state.music_dirs {
        watcher
            .watch(dir, RecursiveMode::Recursive)
            .with_context(|| format!("couldn't watch dir {}", dir.display()))?;
        tracing::info!(
            "File watcher live on {} - changes get auto-indexed incrementally",
            dir.display()
        );
    }

    tokio::task::spawn_blocking(move || {
        loop {
            match rx.recv() {
                Ok(first) => {
                    let mut changed: HashSet<PathBuf> = HashSet::new();
                    collect_event_paths(first, &mut changed);

                    std::thread::sleep(Duration::from_millis(500));
                    while let Ok(ev) = rx.try_recv() {
                        collect_event_paths(ev, &mut changed);
                    }

                    if changed.is_empty() {
                        continue;
                    }
                    let paths: Vec<PathBuf> = changed.into_iter().collect();
                    let touched = paths.len();
                    let count = tokio::runtime::Handle::current()
                        .block_on(perform_incremental_scan(&state, paths));
                    tracing::info!(
                        "Incremental rescan after {} changed path(s): {} tracks indexed",
                        touched,
                        count
                    );
                }
                Err(_) => {
                    tracing::debug!("Watcher channel closed, stopping watcher thread");
                    break;
                }
            }
        }
    });

    Ok(watcher)
}

fn collect_event_paths(res: notify::Result<Event>, changed: &mut HashSet<PathBuf>) {
    match res {
        Ok(event) => changed.extend(event.paths),
        Err(e) => tracing::warn!("File watcher error: {e}"),
    }
}

fn scan_all_libraries(music_dirs: &[PathBuf]) -> HashMap<String, Track> {
    let mut tracks = HashMap::new();
    for (root, dir) in music_dirs.iter().enumerate() {
        for (id, track) in scan_library(root, dir) {
            tracks.insert(id, track);
        }
    }
    tracks
}

fn scan_library(root: usize, music_dir: &Path) -> HashMap<String, Track> {
    let mut tracks = HashMap::new();

    for entry in walkdir::WalkDir::new(music_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();

        if !AUDIO_EXTENSIONS.contains(&ext.as_str()) {
            continue;
        }

        let relative_path = match path.strip_prefix(music_dir) {
            Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };

        match read_track_metadata(root, path, &relative_path, &ext) {
            Ok(track) => {
                tracks.insert(track.id.clone(), track);
            }
            Err(e) => {
                tracing::warn!("Skipped '{}': {e}", path.display());
            }
        }
    }

    tracks
}

fn track_id(root: usize, relative_path: &str) -> String {
    short_hash(&format!("{root}:{relative_path}"))
}

fn file_added_at(path: &Path) -> u64 {
    let metadata = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return now_unix(),
    };
    let system_time = metadata
        .created()
        .or_else(|_| metadata.modified())
        .unwrap_or_else(|_| SystemTime::now());
    system_time
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_else(|_| now_unix())
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_minimal_wav(path: &Path, num_samples: u32) {
        let sample_rate: u32 = 44_100;
        let bits_per_sample: u16 = 16;
        let num_channels: u16 = 1;
        let byte_rate = sample_rate * u32::from(num_channels) * u32::from(bits_per_sample) / 8;
        let block_align = num_channels * bits_per_sample / 8;
        let data_size = num_samples * u32::from(block_align);

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_size).to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&num_channels.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&byte_rate.to_le_bytes());
        bytes.extend_from_slice(&block_align.to_le_bytes());
        bytes.extend_from_slice(&bits_per_sample.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_size.to_le_bytes());
        bytes.extend(std::iter::repeat(0u8).take(data_size as usize));

        fs::write(path, bytes).expect("failed to write test wav file");
    }

    #[test]
    fn recognizes_supported_audio_extensions_case_insensitively() {
        assert!(is_audio_extension("mp3"));
        assert!(is_audio_extension("flac"));
        assert!(is_audio_extension("wav"));
        assert!(!is_audio_extension("MP3")); // callers lowercase before checking
        assert!(!is_audio_extension("txt"));
        assert!(!is_audio_extension(""));
    }

    #[test]
    fn scan_library_indexes_audio_and_skips_everything_else() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_minimal_wav(&dir.path().join("song.wav"), 1000);
        fs::write(dir.path().join("readme.txt"), b"not audio").unwrap();
        fs::create_dir(dir.path().join("Artist")).unwrap();
        write_minimal_wav(&dir.path().join("Artist/another.wav"), 500);

        let tracks = scan_library(0, dir.path());

        assert_eq!(tracks.len(), 2);
        let titles: HashSet<String> = tracks.values().map(|t| t.title.clone()).collect();
        assert!(titles.contains("song"));
        assert!(titles.contains("another"));
    }

    #[test]
    fn untagged_file_falls_back_to_filename_and_folder_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir(dir.path().join("My Album")).unwrap();
        write_minimal_wav(&dir.path().join("My Album/Track One.wav"), 200);

        let tracks = scan_library(0, dir.path());
        assert_eq!(tracks.len(), 1);
        let track = tracks.values().next().unwrap();
        assert_eq!(track.title, "Track One");
        assert_eq!(track.artist, "Unknown Artist");
        assert_eq!(track.album, "My Album");
    }

    #[test]
    fn two_roots_with_identical_relative_paths_dont_collide() {
        let dir_a = tempfile::tempdir().expect("tempdir");
        let dir_b = tempfile::tempdir().expect("tempdir");
        write_minimal_wav(&dir_a.path().join("song.wav"), 100);
        write_minimal_wav(&dir_b.path().join("song.wav"), 100);

        let tracks = scan_all_libraries(&[dir_a.path().to_path_buf(), dir_b.path().to_path_buf()]);
        assert_eq!(tracks.len(), 2);
        let roots: HashSet<usize> = tracks.values().map(|t| t.root).collect();
        assert_eq!(roots, HashSet::from([0, 1]));
    }

    #[test]
    fn apply_path_change_adds_a_new_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut tracks = HashMap::new();

        let file_path = dir.path().join("new_song.wav");
        write_minimal_wav(&file_path, 100);
        apply_path_change(0, dir.path(), &file_path, &mut tracks);

        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks.values().next().unwrap().title, "new_song");
    }

    #[test]
    fn apply_path_change_removes_a_deleted_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("song.wav");
        write_minimal_wav(&file_path, 100);

        let mut tracks = scan_library(0, dir.path());
        assert_eq!(tracks.len(), 1);

        fs::remove_file(&file_path).unwrap();
        apply_path_change(0, dir.path(), &file_path, &mut tracks);

        assert!(tracks.is_empty());
    }

    #[test]
    fn apply_path_change_on_deleted_directory_removes_nested_tracks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let album_dir = dir.path().join("Album");
        fs::create_dir(&album_dir).unwrap();
        write_minimal_wav(&album_dir.join("one.wav"), 100);
        write_minimal_wav(&album_dir.join("two.wav"), 100);
        // a track outside the directory that's about to be deleted
        write_minimal_wav(&dir.path().join("unrelated.wav"), 100);

        let mut tracks = scan_library(0, dir.path());
        assert_eq!(tracks.len(), 3);

        fs::remove_dir_all(&album_dir).unwrap();
        apply_path_change(0, dir.path(), &album_dir, &mut tracks);

        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks.values().next().unwrap().title, "unrelated");
    }

    #[test]
    fn apply_path_change_resyncs_an_existing_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let album_dir = dir.path().join("Album");
        fs::create_dir(&album_dir).unwrap();
        write_minimal_wav(&album_dir.join("one.wav"), 100);

        let mut tracks = scan_library(0, dir.path());
        assert_eq!(tracks.len(), 1);

        // a second file shows up in the same directory
        write_minimal_wav(&album_dir.join("two.wav"), 100);
        apply_path_change(0, dir.path(), &album_dir, &mut tracks);

        assert_eq!(tracks.len(), 2);
    }

    #[test]
    fn preserve_added_at_keeps_earliest_known_timestamp() {
        let mut old = HashMap::new();
        old.insert("abc".to_string(), test_track("abc", 1_000));
        let mut new_tracks = HashMap::new();
        new_tracks.insert("abc".to_string(), test_track("abc", 5_000));

        preserve_added_at(&mut new_tracks, &old);
        assert_eq!(new_tracks["abc"].added_at, 1_000);
    }

    fn test_track(id: &str, added_at: u64) -> Track {
        Track {
            id: id.to_string(),
            title: "t".to_string(),
            artist: "a".to_string(),
            album_artist: None,
            album: "al".to_string(),
            duration_seconds: 1,
            bitrate_kbps: None,
            format: "mp3".to_string(),
            stream_url: String::new(),
            has_embedded_lyrics: false,
            has_embedded_art: false,
            genre: None,
            year: None,
            track_number: None,
            disc_number: None,
            added_at,
            root: 0,
            relative_path: "song.mp3".to_string(),
        }
    }
}

fn read_track_metadata(
    root: usize,
    path: &Path,
    relative_path: &str,
    ext: &str,
) -> anyhow::Result<Track> {
    let probe_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Probe::open(path).and_then(|p| p.read())
    }))
    .map_err(|_| anyhow::anyhow!("tag parser panicked on this file (corrupt/weird file)"))?;

    let tagged_file = probe_result.context("couldn't read audio tags/properties")?;
    let properties = tagged_file.properties();

    let tag = tagged_file
        .primary_tag()
        .or_else(|| tagged_file.first_tag());

    // title fallback: no tag? use the filename without extension
    // lets you play a library with zero tags at all
    let fallback_title = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "Unknown Track".to_string());

    let title = tag
        .and_then(|t| t.title())
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(fallback_title);
    let artist = tag
        .and_then(|t| t.artist())
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "Unknown Artist".to_string());
    let album = tag
        .and_then(|t| t.album())
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            path.parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().to_string())
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "Unknown Album".to_string())
        });

    let album_artist = tag
        .map(|t| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                t.get_string(&ItemKey::AlbumArtist).map(|s| s.to_string())
            }))
            .unwrap_or(None)
        })
        .unwrap_or(None)
        .filter(|s| !s.trim().is_empty());

    let has_embedded_lyrics = tag
        .map(|t| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                t.get_string(&ItemKey::Lyrics)
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false)
            }))
            .unwrap_or(false)
        })
        .unwrap_or(false);

    let has_embedded_art = tag
        .map(|t| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| !t.pictures().is_empty()))
                .unwrap_or(false)
        })
        .unwrap_or(false);

    let genre = tag
        .and_then(|t| t.genre())
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty());
    let year = tag.and_then(|t| t.year());
    let track_number = tag.and_then(|t| t.track());
    let disc_number = tag.and_then(|t| t.disk());

    let id = track_id(root, relative_path);

    Ok(Track {
        id: id.clone(),
        title,
        artist,
        album_artist,
        album,
        duration_seconds: properties.duration().as_secs(),
        bitrate_kbps: properties.audio_bitrate(),
        format: ext.to_string(),
        stream_url: format!("/tracks/{id}/stream"),
        has_embedded_lyrics,
        has_embedded_art,
        genre,
        year,
        track_number,
        disc_number,
        added_at: file_added_at(path),
        root,
        relative_path: relative_path.to_string(),
    })
}

pub(crate) fn is_audio_extension(ext: &str) -> bool {
    AUDIO_EXTENSIONS.contains(&ext)
}
