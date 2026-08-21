use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use axum::{
    Json,
    extract::{Query, State},
    http::HeaderValue,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

use crate::state::{AppState, Track};

#[derive(Debug, Deserialize)]
pub(crate) struct GroupQuery {
    q: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
pub(crate) struct AlbumSummary {
    pub(crate) name: String,
    pub(crate) artist: String,
    pub(crate) is_compilation: bool,
    pub(crate) year: Option<u32>,
    pub(crate) track_count: usize,
    pub(crate) track_ids: Vec<String>,
    pub(crate) art_url: String,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
pub(crate) struct ArtistSummary {
    pub(crate) name: String,
    pub(crate) album_count: usize,
    pub(crate) track_count: usize,
    pub(crate) track_ids: Vec<String>,
}

fn grouping_artist(track: &Track) -> &str {
    track
        .album_artist
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(&track.artist)
}

pub(crate) fn group_albums(tracks: &HashMap<String, Track>) -> Vec<AlbumSummary> {
    let mut albums: HashMap<(String, String), AlbumSummary> = HashMap::new();
    let mut track_artists: HashMap<(String, String), HashSet<String>> = HashMap::new();

    for track in tracks.values() {
        let key = (track.album.clone(), grouping_artist(track).to_string());
        let entry = albums.entry(key.clone()).or_insert_with(|| AlbumSummary {
            name: track.album.clone(),
            artist: key.1.clone(),
            is_compilation: false,
            year: track.year,
            track_count: 0,
            track_ids: Vec::new(),
            art_url: format!("/art/{}", track.id),
        });
        entry.track_count += 1;
        entry.track_ids.push(track.id.clone());
        if entry.year.is_none() {
            entry.year = track.year;
        }
        track_artists
            .entry(key)
            .or_default()
            .insert(track.artist.clone());
    }

    let mut result: Vec<AlbumSummary> = albums.into_values().collect();
    for album in result.iter_mut() {
        album.track_ids.sort();
        let key = (album.name.clone(), album.artist.clone());
        album.is_compilation = track_artists
            .get(&key)
            .map(|set| set.len() > 1)
            .unwrap_or(false);
    }
    result.sort_by(|a, b| a.artist.cmp(&b.artist).then(a.name.cmp(&b.name)));
    result
}

pub(crate) fn group_artists(tracks: &HashMap<String, Track>) -> Vec<ArtistSummary> {
    let mut artists: HashMap<String, ArtistSummary> = HashMap::new();
    let mut album_sets: HashMap<String, HashSet<String>> = HashMap::new();

    for track in tracks.values() {
        let entry = artists
            .entry(track.artist.clone())
            .or_insert_with(|| ArtistSummary {
                name: track.artist.clone(),
                album_count: 0,
                track_count: 0,
                track_ids: Vec::new(),
            });
        entry.track_count += 1;
        entry.track_ids.push(track.id.clone());
        album_sets
            .entry(track.artist.clone())
            .or_default()
            .insert(track.album.clone());
    }

    let mut result: Vec<ArtistSummary> = artists.into_values().collect();
    for artist in result.iter_mut() {
        artist.track_ids.sort();
        artist.album_count = album_sets.get(&artist.name).map(HashSet::len).unwrap_or(0);
    }
    result.sort_by(|a, b| a.name.cmp(&b.name));
    result
}

fn paginate_and_respond<T: Serialize>(
    mut items: Vec<T>,
    query: GroupQuery,
    matches: fn(&T, &str) -> bool,
) -> Response {
    if let Some(q) = &query.q
        && !q.trim().is_empty()
    {
        let needle = q.to_lowercase();
        items.retain(|item| matches(item, &needle));
    }

    let total = items.len();
    let offset = query.offset.unwrap_or(0).min(total);
    let page: Vec<T> = match query.limit {
        Some(limit) => items.into_iter().skip(offset).take(limit).collect(),
        None => items.into_iter().skip(offset).collect(),
    };

    let mut response = Json(page).into_response();
    response.headers_mut().insert(
        "X-Total-Count",
        HeaderValue::from_str(&total.to_string()).unwrap_or_else(|_| HeaderValue::from_static("0")),
    );
    response
}

pub(crate) async fn list_albums(
    State(state): State<Arc<AppState>>,
    Query(query): Query<GroupQuery>,
) -> Response {
    let tracks = state.tracks.read().await;
    let albums = group_albums(&tracks);
    drop(tracks);

    paginate_and_respond(albums, query, |album: &AlbumSummary, needle| {
        album.name.to_lowercase().contains(needle) || album.artist.to_lowercase().contains(needle)
    })
}

pub(crate) async fn list_artists(
    State(state): State<Arc<AppState>>,
    Query(query): Query<GroupQuery>,
) -> Response {
    let tracks = state.tracks.read().await;
    let artists = group_artists(&tracks);
    drop(tracks);

    paginate_and_respond(artists, query, |artist: &ArtistSummary, needle| {
        artist.name.to_lowercase().contains(needle)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str, title: &str, artist: &str, album: &str, year: Option<u32>) -> Track {
        track_with_album_artist(id, title, artist, None, album, year)
    }

    fn track_with_album_artist(
        id: &str,
        title: &str,
        artist: &str,
        album_artist: Option<&str>,
        album: &str,
        year: Option<u32>,
    ) -> Track {
        Track {
            id: id.to_string(),
            title: title.to_string(),
            artist: artist.to_string(),
            album_artist: album_artist.map(|s| s.to_string()),
            album: album.to_string(),
            duration_seconds: 180,
            bitrate_kbps: Some(320),
            format: "mp3".to_string(),
            stream_url: format!("/tracks/{id}/stream"),
            has_embedded_lyrics: false,
            has_embedded_art: false,
            genre: None,
            year,
            track_number: None,
            disc_number: None,
            added_at: 0,
            root: 0,
            relative_path: format!("{artist}/{album}/{title}.mp3"),
        }
    }

    fn sample_tracks() -> HashMap<String, Track> {
        let mut tracks = HashMap::new();
        for t in [
            track("1", "Song A", "Artist X", "Album One", Some(2020)),
            track("2", "Song B", "Artist X", "Album One", Some(2020)),
            track("3", "Song C", "Artist X", "Album Two", Some(2022)),
            track("4", "Song D", "Artist Y", "Album Three", None),
        ] {
            tracks.insert(t.id.clone(), t);
        }
        tracks
    }

    #[test]
    fn groups_tracks_into_albums() {
        let tracks = sample_tracks();
        let albums = group_albums(&tracks);

        assert_eq!(albums.len(), 3);

        let album_one = albums
            .iter()
            .find(|a| a.name == "Album One" && a.artist == "Artist X")
            .expect("Album One should be present");
        assert_eq!(album_one.track_count, 2);
        assert_eq!(album_one.track_ids, vec!["1".to_string(), "2".to_string()]);
        assert_eq!(album_one.year, Some(2020));
        assert!(!album_one.is_compilation);
    }

    #[test]
    fn same_album_name_different_artist_stays_separate() {
        let mut tracks = HashMap::new();
        tracks.insert(
            "1".to_string(),
            track("1", "Song A", "Artist X", "Greatest Hits", Some(2000)),
        );
        tracks.insert(
            "2".to_string(),
            track("2", "Song B", "Artist Y", "Greatest Hits", Some(2010)),
        );

        let albums = group_albums(&tracks);
        assert_eq!(albums.len(), 2);
    }

    #[test]
    fn shared_album_artist_groups_mismatched_track_artists_and_flags_compilation() {
        let mut tracks = HashMap::new();
        tracks.insert(
            "1".to_string(),
            track_with_album_artist(
                "1",
                "Track A",
                "Featured Artist One",
                Some("Various Artists"),
                "Big Compilation",
                Some(2019),
            ),
        );
        tracks.insert(
            "2".to_string(),
            track_with_album_artist(
                "2",
                "Track B",
                "Featured Artist Two",
                Some("Various Artists"),
                "Big Compilation",
                Some(2019),
            ),
        );

        let albums = group_albums(&tracks);
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].artist, "Various Artists");
        assert_eq!(albums[0].track_count, 2);
        assert!(albums[0].is_compilation);
    }

    #[test]
    fn single_artist_album_with_album_artist_tag_is_not_a_compilation() {
        let mut tracks = HashMap::new();
        for (id, title) in [("1", "A"), ("2", "B")] {
            tracks.insert(
                id.to_string(),
                track_with_album_artist(
                    id,
                    title,
                    "Artist X",
                    Some("Artist X"),
                    "Album",
                    Some(2020),
                ),
            );
        }
        let albums = group_albums(&tracks);
        assert_eq!(albums.len(), 1);
        assert!(!albums[0].is_compilation);
    }

    #[test]
    fn albums_are_sorted_by_artist_then_name() {
        let tracks = sample_tracks();
        let albums = group_albums(&tracks);
        let names: Vec<(&str, &str)> = albums
            .iter()
            .map(|a| (a.artist.as_str(), a.name.as_str()))
            .collect();
        assert_eq!(
            names,
            vec![
                ("Artist X", "Album One"),
                ("Artist X", "Album Two"),
                ("Artist Y", "Album Three"),
            ]
        );
    }

    #[test]
    fn groups_tracks_into_artists_with_album_counts() {
        let tracks = sample_tracks();
        let artists = group_artists(&tracks);

        assert_eq!(artists.len(), 2);

        let artist_x = artists
            .iter()
            .find(|a| a.name == "Artist X")
            .expect("Artist X should be present");
        assert_eq!(artist_x.track_count, 3);
        assert_eq!(artist_x.album_count, 2);

        let artist_y = artists
            .iter()
            .find(|a| a.name == "Artist Y")
            .expect("Artist Y should be present");
        assert_eq!(artist_y.track_count, 1);
        assert_eq!(artist_y.album_count, 1);
    }
}
