use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    sync::atomic::AtomicU64,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use tokio::sync::{RwLock, broadcast};

use crate::art::CachedArt;
use crate::library_db::LibraryDb;
use crate::lyrics::LyricsResponse;
use crate::ratelimit::RateLimiter;

// metadata for a single track, returned to API clients
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Track {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) artist: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) album_artist: Option<String>,
    pub(crate) album: String,
    pub(crate) duration_seconds: u64,
    pub(crate) bitrate_kbps: Option<u32>,
    pub(crate) format: String,
    pub(crate) stream_url: String,
    pub(crate) has_embedded_lyrics: bool,
    pub(crate) has_embedded_art: bool,
    pub(crate) genre: Option<String>,
    pub(crate) year: Option<u32>,
    pub(crate) track_number: Option<u32>,
    pub(crate) disc_number: Option<u32>,
    pub(crate) added_at: u64,
    #[serde(skip_serializing)]
    pub(crate) root: usize,
    #[serde(skip_serializing)]
    pub(crate) relative_path: String,
}

pub(crate) struct ScanMeta {
    pub(crate) last_scan: SystemTime,
    pub(crate) duration_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum LibraryEvent {
    Rescanned {
        tracks_total: usize,
        duration_ms: u64,
    },

    TrackUpdated {
        track_id: String,
    },
}

pub(crate) struct AppState {
    pub(crate) tracks: RwLock<HashMap<String, Track>>,

    pub(crate) music_dirs: Vec<PathBuf>,

    pub(crate) music_dir_labels: Vec<String>,
    pub(crate) api_token: Option<String>,
    pub(crate) start_time: Instant,
    pub(crate) scan_meta: RwLock<ScanMeta>,
    pub(crate) request_count: AtomicU64,
    pub(crate) lyrics_cache: RwLock<HashMap<String, LyricsResponse>>,
    pub(crate) art_cache: RwLock<HashMap<String, Option<CachedArt>>>,
    pub(crate) rate_limiter: RateLimiter,
    pub(crate) ffmpeg_available: bool,
    pub(crate) events: broadcast::Sender<LibraryEvent>,
    pub(crate) library_db: Arc<LibraryDb>,
}

impl AppState {
    pub(crate) async fn etag(&self) -> String {
        let meta = self.scan_meta.read().await;
        let secs = meta
            .last_scan
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        format!("\"{secs:x}\"")
    }

    pub(crate) fn track_abs_path(&self, track: &Track) -> PathBuf {
        let base = self
            .music_dirs
            .get(track.root)
            .or_else(|| self.music_dirs.first())
            .cloned()
            .unwrap_or_default();
        base.join(&track.relative_path)
    }

    pub(crate) fn emit(&self, event: LibraryEvent) {
        let _ = self.events.send(event);
    }
}

pub(crate) fn compute_music_dir_labels(dirs: &[PathBuf]) -> Vec<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    let base_names: Vec<String> = dirs
        .iter()
        .map(|d| {
            d.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| d.display().to_string())
        })
        .collect();
    for name in &base_names {
        *counts.entry(name.clone()).or_insert(0) += 1;
    }
    let mut seen: HashMap<String, usize> = HashMap::new();
    base_names
        .into_iter()
        .map(|name| {
            if counts.get(&name).copied().unwrap_or(0) <= 1 {
                name
            } else {
                let n = seen.entry(name.clone()).or_insert(0);
                *n += 1;
                format!("{name} ({n})")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_use_plain_basename_when_unique() {
        let dirs = vec![
            PathBuf::from("/home/x/Music"),
            PathBuf::from("/mnt/nas/Podcasts"),
        ];
        assert_eq!(
            compute_music_dir_labels(&dirs),
            vec!["Music".to_string(), "Podcasts".to_string()]
        );
    }

    #[test]
    fn labels_disambiguate_collisions() {
        let dirs = vec![PathBuf::from("/mnt/a/Music"), PathBuf::from("/mnt/b/Music")];
        assert_eq!(
            compute_music_dir_labels(&dirs),
            vec!["Music (1)".to_string(), "Music (2)".to_string()]
        );
    }
}
