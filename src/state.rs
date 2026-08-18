use std::{
    collections::HashMap,
    path::PathBuf,
    sync::atomic::AtomicU64,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use tokio::sync::RwLock;

use crate::art::CachedArt;
use crate::lyrics::LyricsResponse;

// metadata for a single track, returned to API clients
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Track {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) album: String,
    pub(crate) duration_seconds: u64,
    pub(crate) bitrate_kbps: Option<u32>,
    pub(crate) format: String,
    pub(crate) stream_url: String,
    pub(crate) has_embedded_lyrics: bool,
    pub(crate) has_embedded_art: bool,
    #[serde(skip_serializing)]
    pub(crate) relative_path: String,
}

pub(crate) struct ScanMeta {
    pub(crate) last_scan: SystemTime,
    pub(crate) duration_ms: u64,
}

pub(crate) struct AppState {
    pub(crate) tracks: RwLock<HashMap<String, Track>>,
    pub(crate) music_dir: PathBuf,
    pub(crate) api_token: Option<String>,
    pub(crate) start_time: Instant,
    pub(crate) scan_meta: RwLock<ScanMeta>,
    pub(crate) request_count: AtomicU64,
    pub(crate) lyrics_cache: RwLock<HashMap<String, LyricsResponse>>,
    pub(crate) art_cache: RwLock<HashMap<String, Option<CachedArt>>>,
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
}
