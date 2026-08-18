use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{
    body::Body,
    extract::{Path as AxumPath, Query, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use lofty::{prelude::*, probe::Probe};
use serde::Deserialize;

use crate::state::AppState;

const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp"];

const FOLDER_ART_NAMES: &[&str] = &[
    "cover.jpg",
    "cover.jpeg",
    "cover.png",
    "cover.webp",
    "folder.jpg",
    "folder.jpeg",
    "folder.png",
    "folder.webp",
    "front.jpg",
    "front.jpeg",
    "front.png",
    "front.webp",
];

const PLACEHOLDER_ART_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 300 300">
  <rect width="300" height="300" fill="#2a2a2e"/>
  <path d="M120 210c0 16.6-13.4 30-30 30s-30-13.4-30-30 13.4-30 30-30c5.5 0 10.6 1.5 15 4V70l120-24v130c0 16.6-13.4 30-30 30s-30-13.4-30-30 13.4-30 30-30c5.5 0 10.6 1.5 15 4V96l-90 18v96z" fill="#6b6b72"/>
</svg>"##;

#[derive(Clone)]
pub(crate) struct CachedArt {
    bytes: Arc<Vec<u8>>,
    mime: String,
    source: &'static str,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ArtQuery {
    placeholder: Option<bool>,
}

pub(crate) async fn get_art(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<ArtQuery>,
) -> Response {
    let want_placeholder = query.placeholder.unwrap_or(true);

    if let Some(cached) = state.art_cache.read().await.get(&id).cloned() {
        return render_art_response(cached, want_placeholder);
    }

    let track = {
        let tracks = state.tracks.read().await;
        match tracks.get(&id) {
            Some(t) => t.clone(),
            None => return StatusCode::NOT_FOUND.into_response(),
        }
    };

    let full_path = state.music_dir.join(&track.relative_path);
    let cached = tokio::task::spawn_blocking(move || resolve_art(&full_path))
        .await
        .unwrap_or_else(|e| {
            tracing::warn!("Art lookup thread panicked: {e}");
            None
        });

    state.art_cache.write().await.insert(id, cached.clone());
    render_art_response(cached, want_placeholder)
}

fn render_art_response(cached: Option<CachedArt>, want_placeholder: bool) -> Response {
    match cached {
        Some(art) => {
            let mut response = Response::new(Body::from((*art.bytes).clone()));
            let headers = response.headers_mut();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_str(&art.mime)
                    .unwrap_or_else(|_| HeaderValue::from_static("image/jpeg")),
            );
            headers.insert("X-Art-Source", HeaderValue::from_static(art.source));
            headers.insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=86400"),
            );
            response
        }
        None if want_placeholder => (
            StatusCode::OK,
            [
                ("content-type", "image/svg+xml"),
                ("x-art-source", "placeholder"),
            ],
            PLACEHOLDER_ART_SVG,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn resolve_art(audio_path: &Path) -> Option<CachedArt> {
    let dir = audio_path.parent().unwrap_or(audio_path);
    let stem = audio_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");

    if let Some((bytes, mime)) = try_embedded_art(audio_path) {
        return Some(CachedArt {
            bytes: Arc::new(bytes),
            mime,
            source: "embedded",
        });
    }
    if let Some(path) = try_folder_art(dir) {
        if let Some((bytes, mime)) = read_image_file(&path) {
            return Some(CachedArt {
                bytes: Arc::new(bytes),
                mime,
                source: "folder",
            });
        }
    }
    if let Some(path) = try_track_named_art(dir, stem) {
        if let Some((bytes, mime)) = read_image_file(&path) {
            return Some(CachedArt {
                bytes: Arc::new(bytes),
                mime,
                source: "track_named",
            });
        }
    }
    None
}

fn try_embedded_art(path: &Path) -> Option<(Vec<u8>, String)> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let tagged = Probe::open(path).ok()?.read().ok()?;
        let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
        let picture = tag.pictures().first()?;
        let mime = picture
            .mime_type()
            .map(|m| m.to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "image/jpeg".to_string());
        Some((picture.data().to_vec(), mime))
    }))
    .unwrap_or(None)
}

fn try_folder_art(dir: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name_lower = path.file_name()?.to_str()?.to_lowercase();
        if FOLDER_ART_NAMES.contains(&name_lower.as_str()) {
            return Some(path);
        }
    }
    None
}

fn try_track_named_art(dir: &Path, stem: &str) -> Option<PathBuf> {
    if stem.is_empty() {
        return None;
    }
    for ext in IMAGE_EXTENSIONS {
        let candidate = dir.join(format!("{stem}.{ext}"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn read_image_file(path: &Path) -> Option<(Vec<u8>, String)> {
    let bytes = fs::read(path).ok()?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "webp" => "image/webp",
        _ => "image/jpeg",
    }
    .to_string();
    Some((bytes, mime))
}
