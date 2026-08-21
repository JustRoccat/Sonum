use std::{
    net::SocketAddr,
    sync::{Arc, atomic::Ordering},
    time::{Duration, SystemTime},
};

use axum::{
    Json,
    body::Body,
    extract::{ConnectInfo, Path as AxumPath, Query, State},
    http::{HeaderMap, HeaderValue, Request, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::Deserialize;

use crate::ratelimit::Decision;
use crate::scan::perform_scan;
use crate::state::{AppState, Track};
use crate::transcode::{self, TranscodeFormat};

const MIN_RESCAN_INTERVAL: Duration = Duration::from_secs(10);
const MAX_PAGE_LIMIT: usize = 2000;

#[derive(Debug, Deserialize)]
pub(crate) struct ListQuery {
    q: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
    sort: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct StreamQuery {
    format: Option<String>,
    bitrate: Option<u32>,
}

pub(crate) async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> impl IntoResponse {
    if let Some(expected) = &state.api_token {
        let ok = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(|v| v == format!("Bearer {expected}"))
            .unwrap_or(false);

        if !ok {
            return (StatusCode::UNAUTHORIZED, "Invalid or missing token").into_response();
        }
    }
    next.run(request).await
}

// counts handled requests for /metrics
pub(crate) async fn count_requests_middleware(
    State(state): State<Arc<AppState>>,
    request: Request<Body>,
    next: axum::middleware::Next,
) -> impl IntoResponse {
    state.request_count.fetch_add(1, Ordering::Relaxed);
    next.run(request).await
}

pub(crate) async fn rate_limit_middleware(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: Request<Body>,
    next: axum::middleware::Next,
) -> Response {
    match state.rate_limiter.check(addr.ip()) {
        Decision::Allow => next.run(request).await,
        Decision::Deny { retry_after_secs } => {
            let mut response = (
                StatusCode::TOO_MANY_REQUESTS,
                "Rate limit exceeded, slow down.",
            )
                .into_response();
            response.headers_mut().insert(
                header::RETRY_AFTER,
                HeaderValue::from_str(&retry_after_secs.to_string())
                    .unwrap_or_else(|_| HeaderValue::from_static("1")),
            );
            response
        }
    }
}

fn sort_tracks(tracks: &mut [Track], sort: Option<&str>) {
    match sort {
        Some("added_desc") => tracks.sort_by_key(|t| std::cmp::Reverse(t.added_at)),
        Some("added_asc") => tracks.sort_by_key(|t| t.added_at),
        Some("title") => tracks.sort_by(|a, b| a.title.cmp(&b.title)),
        // "artist" and anything unrecognized both fall back to the
        // original default ordering.
        _ => tracks.sort_by(|a, b| a.artist.cmp(&b.artist).then(a.title.cmp(&b.title))),
    }
}

pub(crate) async fn list_tracks(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ListQuery>,
    headers: HeaderMap,
) -> Response {
    let etag = state.etag().await;
    if let Some(inm) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        && inm == etag
    {
        return StatusCode::NOT_MODIFIED.into_response();
    }

    let tracks = state.tracks.read().await;
    let mut result: Vec<Track> = match &query.q {
        Some(q) if !q.trim().is_empty() => {
            let needle = q.to_lowercase();
            tracks
                .values()
                .filter(|t| {
                    t.title.to_lowercase().contains(&needle)
                        || t.artist.to_lowercase().contains(&needle)
                        || t.album.to_lowercase().contains(&needle)
                })
                .cloned()
                .collect()
        }
        _ => tracks.values().cloned().collect(),
    };
    sort_tracks(&mut result, query.sort.as_deref());
    drop(tracks);

    let total = result.len();
    let offset = query.offset.unwrap_or(0).min(total);
    let limit = query.limit.unwrap_or(usize::MAX).min(MAX_PAGE_LIMIT);
    let page: Vec<Track> = result.into_iter().skip(offset).take(limit).collect();

    let last_modified = httpdate::fmt_http_date(state.scan_meta.read().await.last_scan);

    let mut response = Json(page).into_response();
    let hm = response.headers_mut();
    hm.insert(
        "X-Total-Count",
        HeaderValue::from_str(&total.to_string()).unwrap_or_else(|_| HeaderValue::from_static("0")),
    );
    hm.insert(
        header::ETAG,
        HeaderValue::from_str(&etag).unwrap_or_else(|_| HeaderValue::from_static("\"0\"")),
    );
    hm.insert(
        header::LAST_MODIFIED,
        HeaderValue::from_str(&last_modified).unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    response
}

pub(crate) async fn get_track(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Track>, StatusCode> {
    let tracks = state.tracks.read().await;
    tracks
        .get(&id)
        .cloned()
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

pub(crate) async fn stream_track(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<StreamQuery>,
    headers: HeaderMap,
) -> Response {
    let track = {
        let tracks = state.tracks.read().await;
        match tracks.get(&id) {
            Some(t) => t.clone(),
            None => return StatusCode::NOT_FOUND.into_response(),
        }
    };

    let Some(format_str) = query.format else {
        let encoded_path: String = track
            .relative_path
            .split('/')
            .map(|segment| utf8_percent_encode(segment, NON_ALPHANUMERIC).to_string())
            .collect::<Vec<_>>()
            .join("/");
        return Redirect::temporary(&format!("/files/{}/{encoded_path}", track.root))
            .into_response();
    };

    let Some(format) = TranscodeFormat::parse(&format_str) else {
        return (
            StatusCode::BAD_REQUEST,
            "Unsupported 'format'. Use one of: mp3, opus, aac.",
        )
            .into_response();
    };

    if !state.ffmpeg_available {
        return (
            StatusCode::NOT_IMPLEMENTED,
            "Transcoding requires ffmpeg, which isn't available on this server.",
        )
            .into_response();
    }

    let bitrate = transcode::clamp_bitrate(query.bitrate);
    let range_start = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(transcode::parse_range_start);
    let seek_seconds = range_start.map(|b| transcode::byte_range_to_seek_seconds(b, bitrate));

    let source_path = state.track_abs_path(&track);
    match transcode::transcode(&source_path, format, query.bitrate, seek_seconds).await {
        Ok(mut response) => {
            if let Some(start) = range_start {
                *response.status_mut() = StatusCode::PARTIAL_CONTENT;

                response.headers_mut().insert(
                    header::CONTENT_RANGE,
                    HeaderValue::from_str(&format!("bytes {start}-*/*"))
                        .unwrap_or_else(|_| HeaderValue::from_static("bytes */*")),
                );
            }
            response
        }
        Err(e) => {
            tracing::error!("Transcode of track {id} failed to start: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to start transcoding this track.",
            )
                .into_response()
        }
    }
}

pub(crate) async fn rescan(State(state): State<Arc<AppState>>) -> Response {
    let last_scan = state.scan_meta.read().await.last_scan;
    if let Ok(elapsed) = SystemTime::now().duration_since(last_scan)
        && elapsed < MIN_RESCAN_INTERVAL
    {
        let retry_after = (MIN_RESCAN_INTERVAL - elapsed).as_secs().max(1);
        let mut response = (
            StatusCode::TOO_MANY_REQUESTS,
            "Rescan requested too recently, try again shortly.",
        )
            .into_response();
        response.headers_mut().insert(
            header::RETRY_AFTER,
            HeaderValue::from_str(&retry_after.to_string())
                .unwrap_or_else(|_| HeaderValue::from_static("1")),
        );
        return response;
    }

    let count = perform_scan(&state).await;
    Json(serde_json::json!({ "rescanned": true, "tracks_found": count })).into_response()
}

pub(crate) async fn health(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let tracks_indexed = state.tracks.read().await.len();
    Json(serde_json::json!({
        "status": "ok",
        "uptime_seconds": state.start_time.elapsed().as_secs(),
        "tracks_indexed": tracks_indexed,
        "music_dirs": state.music_dirs.len(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str, artist: &str, title: &str, added_at: u64) -> Track {
        Track {
            id: id.to_string(),
            title: title.to_string(),
            artist: artist.to_string(),
            album_artist: None,
            album: "Album".to_string(),
            duration_seconds: 100,
            bitrate_kbps: Some(320),
            format: "mp3".to_string(),
            stream_url: format!("/tracks/{id}/stream"),
            has_embedded_lyrics: false,
            has_embedded_art: false,
            genre: None,
            year: None,
            track_number: None,
            disc_number: None,
            added_at,
            root: 0,
            relative_path: format!("{artist}/{title}.mp3"),
        }
    }

    #[test]
    fn default_sort_is_artist_then_title() {
        let mut tracks = vec![
            track("1", "B Artist", "Z", 1),
            track("2", "A Artist", "Z", 2),
        ];
        sort_tracks(&mut tracks, None);
        assert_eq!(tracks[0].artist, "A Artist");
    }

    #[test]
    fn added_desc_sorts_newest_first() {
        let mut tracks = vec![
            track("1", "A", "T", 10),
            track("2", "A", "T", 30),
            track("3", "A", "T", 20),
        ];
        sort_tracks(&mut tracks, Some("added_desc"));
        let order: Vec<u64> = tracks.iter().map(|t| t.added_at).collect();
        assert_eq!(order, vec![30, 20, 10]);
    }

    #[test]
    fn added_asc_sorts_oldest_first() {
        let mut tracks = vec![
            track("1", "A", "T", 10),
            track("2", "A", "T", 30),
            track("3", "A", "T", 20),
        ];
        sort_tracks(&mut tracks, Some("added_asc"));
        let order: Vec<u64> = tracks.iter().map(|t| t.added_at).collect();
        assert_eq!(order, vec![10, 20, 30]);
    }

    #[test]
    fn unknown_sort_value_falls_back_to_default() {
        let mut tracks = vec![
            track("1", "B Artist", "Z", 1),
            track("2", "A Artist", "Z", 2),
        ];
        sort_tracks(&mut tracks, Some("nonsense"));
        assert_eq!(tracks[0].artist, "A Artist");
    }
}
