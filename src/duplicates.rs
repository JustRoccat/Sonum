use std::{collections::HashMap, sync::Arc};

use axum::{Json, extract::State};
use serde::Serialize;

use crate::state::{AppState, Track};
use crate::util::normalize_for_match;

const DURATION_TOLERANCE_SECS: i64 = 3;

#[derive(Debug, Serialize)]
pub(crate) struct DuplicateGroup {
    pub(crate) reason: &'static str,
    pub(crate) tracks: Vec<Track>,
}

pub(crate) fn find_duplicates(tracks: &HashMap<String, Track>) -> Vec<DuplicateGroup> {
    let mut buckets: HashMap<(String, String), Vec<&Track>> = HashMap::new();
    for track in tracks.values() {
        let key = (
            normalize_for_match(&track.title),
            normalize_for_match(&track.artist),
        );
        if key.0.is_empty() {
            continue; // nothing meaningful to match on
        }
        buckets.entry(key).or_default().push(track);
    }

    let mut groups: Vec<DuplicateGroup> = Vec::new();
    for mut bucket in buckets.into_values() {
        if bucket.len() < 2 {
            continue;
        }
        bucket.sort_by_key(|t| t.duration_seconds);

        // greedily split the duration sorted bucket into runs where
        // consecutive tracks are within tolerance of each other.
        let mut run: Vec<&Track> = vec![bucket[0]];
        for pair in bucket.windows(2) {
            let (prev, next) = (pair[0], pair[1]);
            let diff = (next.duration_seconds as i64 - prev.duration_seconds as i64).abs();
            if diff <= DURATION_TOLERANCE_SECS {
                run.push(next);
            } else {
                if run.len() > 1 {
                    groups.push(DuplicateGroup {
                        reason: "same title/artist, similar duration",
                        tracks: run.iter().map(|t| (*t).clone()).collect(),
                    });
                }
                run = vec![next];
            }
        }
        if run.len() > 1 {
            groups.push(DuplicateGroup {
                reason: "same title/artist, similar duration",
                tracks: run.into_iter().cloned().collect(),
            });
        }
    }

    groups.sort_by(|a, b| {
        let a_key = a
            .tracks
            .first()
            .map(|t| t.title.clone())
            .unwrap_or_default();
        let b_key = b
            .tracks
            .first()
            .map(|t| t.title.clone())
            .unwrap_or_default();
        a_key.cmp(&b_key)
    });
    groups
}

pub(crate) async fn list_duplicates(
    State(state): State<Arc<AppState>>,
) -> Json<Vec<DuplicateGroup>> {
    let tracks = state.tracks.read().await;
    Json(find_duplicates(&tracks))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(
        id: &str,
        title: &str,
        artist: &str,
        duration_seconds: u64,
        relative_path: &str,
    ) -> Track {
        Track {
            id: id.to_string(),
            title: title.to_string(),
            artist: artist.to_string(),
            album_artist: None,
            album: "Album".to_string(),
            duration_seconds,
            bitrate_kbps: Some(320),
            format: "mp3".to_string(),
            stream_url: format!("/tracks/{id}/stream"),
            has_embedded_lyrics: false,
            has_embedded_art: false,
            genre: None,
            year: None,
            track_number: None,
            disc_number: None,
            added_at: 0,
            root: 0,
            relative_path: relative_path.to_string(),
        }
    }

    #[test]
    fn flags_same_title_artist_and_close_duration_as_duplicates() {
        let mut tracks = HashMap::new();
        tracks.insert(
            "1".to_string(),
            track("1", "Song", "Artist", 200, "a/song.mp3"),
        );
        tracks.insert(
            "2".to_string(),
            track("2", "song (Remaster)".trim(), "artist", 201, "b/song.flac"),
        );
        // note: "song (Remaster)" normalizes differently than "Song" due to
        // the extra word, so use identical normalized text for this case
        tracks.insert(
            "2".to_string(),
            track("2", "Song", "Artist", 201, "b/song.flac"),
        );

        let groups = find_duplicates(&tracks);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].tracks.len(), 2);
    }

    #[test]
    fn different_duration_is_not_flagged() {
        let mut tracks = HashMap::new();
        tracks.insert("1".to_string(), track("1", "Song", "Artist", 100, "a.mp3"));
        tracks.insert("2".to_string(), track("2", "Song", "Artist", 400, "b.mp3"));

        assert!(find_duplicates(&tracks).is_empty());
    }

    #[test]
    fn unique_tracks_produce_no_groups() {
        let mut tracks = HashMap::new();
        tracks.insert(
            "1".to_string(),
            track("1", "Song A", "Artist", 100, "a.mp3"),
        );
        tracks.insert(
            "2".to_string(),
            track("2", "Song B", "Artist", 100, "b.mp3"),
        );

        assert!(find_duplicates(&tracks).is_empty());
    }
}
