use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use lofty::{prelude::*, probe::Probe, tag::ItemKey};
use serde::Serialize;

use crate::state::AppState;
use crate::util::parent_rel;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LyricLine {
    time_ms: u64,
    text: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LyricsResponse {
    track_id: String,
    source: &'static str,
    synced: bool,
    lines: Option<Vec<LyricLine>>,
    text: Option<String>,
}

impl LyricsResponse {
    fn none(track_id: &str) -> Self {
        Self {
            track_id: track_id.to_string(),
            source: "none",
            synced: false,
            lines: None,
            text: None,
        }
    }

    fn from_parsed(track_id: &str, source: &'static str, parsed: ParsedLyrics) -> Self {
        if parsed.text.trim().is_empty() {
            return Self::none(track_id);
        }
        Self {
            track_id: track_id.to_string(),
            source,
            synced: parsed.synced,
            lines: if parsed.synced {
                Some(parsed.lines)
            } else {
                None
            },
            text: Some(parsed.text),
        }
    }
}

// result of parsing any lyrics content (embedded or .lrc file)
struct ParsedLyrics {
    synced: bool,
    lines: Vec<LyricLine>,
    text: String,
}

pub(crate) async fn get_lyrics(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    if let Some(cached) = state.lyrics_cache.read().await.get(&id).cloned() {
        return Json(cached).into_response();
    }

    let track = {
        let tracks = state.tracks.read().await;
        match tracks.get(&id) {
            Some(t) => t.clone(),
            None => return StatusCode::NOT_FOUND.into_response(),
        }
    };

    let full_path = state.music_dir.join(&track.relative_path);
    let dir = full_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| state.music_dir.clone());
    let stem = full_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();

    let sibling_audio_count = {
        let tracks = state.tracks.read().await;
        let this_dir = parent_rel(&track.relative_path);
        tracks
            .values()
            .filter(|t| parent_rel(&t.relative_path) == this_dir)
            .count()
    };

    let response = tokio::task::spawn_blocking(move || {
        resolve_lyrics(&id, &full_path, &dir, &stem, sibling_audio_count)
    })
    .await
    .unwrap_or_else(|e| {
        tracing::warn!("Lyrics lookup thread panicked: {e}");
        LyricsResponse::none("")
    });

    state
        .lyrics_cache
        .write()
        .await
        .insert(response.track_id.clone(), response.clone());

    Json(response).into_response()
}

fn resolve_lyrics(
    id: &str,
    audio_path: &Path,
    dir: &Path,
    stem: &str,
    sibling_audio_count: usize,
) -> LyricsResponse {
    if let Some(raw) = try_embedded_lyrics(audio_path) {
        let parsed = parse_lyrics_content(&raw);
        let resp = LyricsResponse::from_parsed(id, "embedded", parsed);
        if resp.source != "none" {
            return resp;
        }
    }

    if let Some(path) = find_case_insensitive_sibling(dir, stem, "lrc") {
        if let Some(raw) = read_text_file_lossy(&path) {
            let parsed = parse_lyrics_content(&raw);
            let resp = LyricsResponse::from_parsed(id, "track_lrc", parsed);
            if resp.source != "none" {
                return resp;
            }
        }
    }

    if sibling_audio_count == 1 {
        if let Some(path) = find_shared_lrc_candidate(dir) {
            if let Some(raw) = read_text_file_lossy(&path) {
                let parsed = parse_lyrics_content(&raw);
                let resp = LyricsResponse::from_parsed(id, "shared_lrc", parsed);
                if resp.source != "none" {
                    return resp;
                }
            }
        }
    }

    LyricsResponse::none(id)
}

fn try_embedded_lyrics(path: &Path) -> Option<String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let tagged = Probe::open(path).ok()?.read().ok()?;
        let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
        tag.get_string(&ItemKey::Lyrics)
            .map(|s| s.to_string())
            .filter(|s| !s.trim().is_empty())
    }))
    .unwrap_or(None)
}

fn find_case_insensitive_sibling(dir: &Path, stem: &str, ext: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    let stem_lower = stem.to_lowercase();
    let ext_lower = ext.to_lowercase();
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let file_stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();
        let file_ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();
        if file_stem == stem_lower && file_ext == ext_lower {
            return Some(path);
        }
    }
    None
}

fn find_shared_lrc_candidate(dir: &Path) -> Option<PathBuf> {
    let folder_name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_lowercase());

    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .as_deref()
            != Some("lrc")
        {
            continue;
        }
        let name_lower = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();
        if name_lower == "lyrics" || Some(name_lower.as_str()) == folder_name.as_deref() {
            return Some(path);
        }
    }
    None
}

fn read_text_file_lossy(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn parse_lyrics_content(content: &str) -> ParsedLyrics {
    let synced_lines = parse_lrc_lines(content);
    if !synced_lines.is_empty() {
        let text = synced_lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        return ParsedLyrics {
            synced: true,
            lines: synced_lines,
            text,
        };
    }

    let plain: String = content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !is_lrc_metadata_header(line))
        .collect::<Vec<_>>()
        .join("\n");

    ParsedLyrics {
        synced: false,
        lines: Vec::new(),
        text: plain,
    }
}

fn is_lrc_metadata_header(line: &str) -> bool {
    line.starts_with('[')
        && line.ends_with(']')
        && line.len() >= 4
        && line[1..3].chars().all(|c| c.is_ascii_alphabetic())
        && line.as_bytes().get(3) == Some(&b':')
}

fn parse_lrc_lines(content: &str) -> Vec<LyricLine> {
    let mut out = Vec::new();
    for raw_line in content.lines() {
        let mut rest = raw_line.trim();
        let mut timestamps: Vec<u64> = Vec::new();

        while let Some(stripped) = rest.strip_prefix('[') {
            let Some(close) = stripped.find(']') else {
                break;
            };
            let tag = &stripped[..close];
            match parse_lrc_timestamp(tag) {
                Some(ms) => {
                    timestamps.push(ms);
                    rest = &stripped[close + 1..];
                }
                None => break,
            }
        }

        if timestamps.is_empty() {
            continue;
        }

        let text = rest.trim().to_string();
        for ms in timestamps {
            out.push(LyricLine {
                time_ms: ms,
                text: text.clone(),
            });
        }
    }
    out.sort_by_key(|l| l.time_ms);
    out
}

fn parse_lrc_timestamp(tag: &str) -> Option<u64> {
    let (min_str, rest) = tag.split_once(':')?;
    let minutes: u64 = min_str.trim().parse().ok()?;

    let (sec_str, frac_str) = match rest.split_once(['.', ':']) {
        Some((s, f)) => (s, Some(f)),
        None => (rest, None),
    };
    let seconds: u64 = sec_str.trim().parse().ok()?;
    if seconds >= 60 {
        return None;
    }

    let frac_ms = match frac_str {
        Some(f) if !f.trim().is_empty() => {
            let f = f.trim();
            let value: u64 = f.parse().ok()?;
            match f.len() {
                1 => value * 100,
                2 => value * 10,
                3 => value,
                _ => value % 1000,
            }
        }
        _ => 0,
    };

    Some(minutes * 60_000 + seconds * 1000 + frac_ms)
}
