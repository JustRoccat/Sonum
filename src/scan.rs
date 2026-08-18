use std::{
    collections::HashMap,
    path::Path,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use anyhow::Context;
use lofty::{file::AudioFile, prelude::*, probe::Probe, tag::ItemKey};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

use crate::state::{AppState, ScanMeta, Track};
use crate::util::short_hash;

const AUDIO_EXTENSIONS: &[&str] = &["mp3", "flac", "ogg", "opus", "m4a", "wav", "aac"];

pub(crate) fn timed_scan(music_dir: &Path) -> (HashMap<String, Track>, Duration) {
    let started = Instant::now();
    let tracks = scan_library(music_dir);
    (tracks, started.elapsed())
}

pub(crate) async fn perform_scan(state: &Arc<AppState>) -> usize {
    let music_dir = state.music_dir.clone();
    let (new_tracks, duration) = tokio::task::spawn_blocking(move || timed_scan(&music_dir))
        .await
        .unwrap_or_else(|e| {
            tracing::error!("background scan blew up: {e}");
            (HashMap::new(), Duration::default())
        });

    let count = new_tracks.len();
    *state.tracks.write().await = new_tracks;
    *state.scan_meta.write().await = ScanMeta {
        last_scan: SystemTime::now(),
        duration_ms: duration.as_millis() as u64,
    };

    state.lyrics_cache.write().await.clear();
    state.art_cache.write().await.clear();
    count
}

pub(crate) fn spawn_music_dir_watcher(state: Arc<AppState>) -> anyhow::Result<RecommendedWatcher> {
    let (tx, rx) = std::sync::mpsc::channel::<notify::Result<Event>>();

    let mut watcher = notify::recommended_watcher(move |res| {
        // notify callback runs on its own thread, just forward the event
        let _ = tx.send(res);
    })
    .context("couldn't set up the file watcher (notify)")?;

    watcher
        .watch(&state.music_dir, RecursiveMode::Recursive)
        .with_context(|| format!("couldn't watch dir {}", state.music_dir.display()))?;

    tracing::info!(
        "File watcher live on {} - changes get auto-indexed",
        state.music_dir.display()
    );

    tokio::task::spawn_blocking(move || loop {
        match rx.recv() {
            Ok(Ok(_event)) => {
                while rx.try_recv().is_ok() {}
                std::thread::sleep(Duration::from_millis(500));
                while rx.try_recv().is_ok() {}

                let count = tokio::runtime::Handle::current().block_on(perform_scan(&state));
                tracing::info!("Auto-rescan after music_dir change: {} tracks", count);
            }
            Ok(Err(e)) => tracing::warn!("File watcher error: {e}"),
            Err(_) => {
                tracing::debug!("Watcher channel closed, stopping watcher thread");
                break;
            }
        }
    });

    Ok(watcher)
}

fn scan_library(music_dir: &Path) -> HashMap<String, Track> {
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

        match read_track_metadata(path, &relative_path, &ext) {
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

fn read_track_metadata(path: &Path, relative_path: &str, ext: &str) -> anyhow::Result<Track> {
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

    let id = short_hash(relative_path);

    Ok(Track {
        id: id.clone(),
        title,
        artist,
        album,
        duration_seconds: properties.duration().as_secs(),
        bitrate_kbps: properties.audio_bitrate(),
        format: ext.to_string(),
        stream_url: format!("/tracks/{id}/stream"),
        has_embedded_lyrics,
        has_embedded_art,
        relative_path: relative_path.to_string(),
    })
}
