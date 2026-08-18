use std::{fs, sync::atomic::Ordering, sync::Arc};

use axum::{extract::State, http::header, response::IntoResponse};

use crate::state::AppState;

pub(crate) async fn metrics(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let tracks_indexed = state.tracks.read().await.len();
    let uptime = state.start_time.elapsed().as_secs();
    let requests = state.request_count.load(Ordering::Relaxed);
    let scan_meta = state.scan_meta.read().await;
    let rss_bytes = current_rss_bytes().unwrap_or(0);

    let body = format!(
        "# HELP sonum_uptime_seconds Server uptime in seconds\n\
         # TYPE sonum_uptime_seconds counter\n\
         sonum_uptime_seconds {uptime}\n\
         \n\
         # HELP sonum_tracks_indexed Number of currently indexed tracks\n\
         # TYPE sonum_tracks_indexed gauge\n\
         sonum_tracks_indexed {tracks_indexed}\n\
         \n\
         # HELP sonum_requests_total Total HTTP requests handled\n\
         # TYPE sonum_requests_total counter\n\
         sonum_requests_total {requests}\n\
         \n\
         # HELP sonum_memory_rss_bytes RSS memory usage in bytes\n\
         # TYPE sonum_memory_rss_bytes gauge\n\
         sonum_memory_rss_bytes {rss_bytes}\n\
         \n\
         # HELP sonum_last_scan_duration_ms Duration of the last library scan in ms\n\
         # TYPE sonum_last_scan_duration_ms gauge\n\
         sonum_last_scan_duration_ms {}\n",
        scan_meta.duration_ms
    );

    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
}

fn current_rss_bytes() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest.trim().trim_end_matches(" kB").trim().parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}
