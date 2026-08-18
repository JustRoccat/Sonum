use std::sync::{atomic::Ordering, Arc};

use axum::{
    body::Body,
    extract::{Path as AxumPath, Query, State},
    http::{header, HeaderMap, HeaderValue, Request, StatusCode},
    response::{IntoResponse, Redirect, Response},
    Json,
};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde::Deserialize;

use crate::scan::perform_scan;
use crate::state::{AppState, Track};

#[derive(Debug, Deserialize)]
pub(crate) struct ListQuery {
    q: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
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

pub(crate) async fn list_tracks(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ListQuery>,
    headers: HeaderMap,
) -> Response {
    let etag = state.etag().await;
    if let Some(inm) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
    {
        if inm == etag {
            return StatusCode::NOT_MODIFIED.into_response();
        }
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
    result.sort_by(|a, b| a.artist.cmp(&b.artist).then(a.title.cmp(&b.title)));
    drop(tracks);

    let total = result.len();
    let offset = query.offset.unwrap_or(0).min(total);
    let page: Vec<Track> = match query.limit {
        Some(limit) => result.into_iter().skip(offset).take(limit).collect(),
        None => result.into_iter().skip(offset).collect(),
    };

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
) -> Result<Redirect, StatusCode> {
    let tracks = state.tracks.read().await;
    let track = tracks.get(&id).ok_or(StatusCode::NOT_FOUND)?;

    let encoded_path: String = track
        .relative_path
        .split('/')
        .map(|segment| utf8_percent_encode(segment, NON_ALPHANUMERIC).to_string())
        .collect::<Vec<_>>()
        .join("/");

    Ok(Redirect::temporary(&format!("/files/{encoded_path}")))
}

pub(crate) async fn rescan(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let count = perform_scan(&state).await;
    Json(serde_json::json!({ "rescanned": true, "tracks_found": count }))
}

pub(crate) async fn health(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let tracks_indexed = state.tracks.read().await.len();
    Json(serde_json::json!({
        "status": "ok",
        "uptime_seconds": state.start_time.elapsed().as_secs(),
        "tracks_indexed": tracks_indexed,
    }))
}
