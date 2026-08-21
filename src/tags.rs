use std::{path::Path, sync::Arc};

use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use lofty::{
    config::WriteOptions,
    file::{AudioFile, TaggedFileExt},
    prelude::*,
    probe::Probe,
    tag::{ItemKey, Tag},
};
use serde::Deserialize;

use crate::library_db::fingerprint_file;
use crate::state::{AppState, LibraryEvent, Track};

#[derive(Debug, Deserialize, Default)]
pub(crate) struct TagEdits {
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    album_artist: Option<String>,
    genre: Option<String>,
    year: Option<u32>,
    track_number: Option<u32>,
    disc_number: Option<u32>,
}

impl TagEdits {
    fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.artist.is_none()
            && self.album.is_none()
            && self.album_artist.is_none()
            && self.genre.is_none()
            && self.year.is_none()
            && self.track_number.is_none()
            && self.disc_number.is_none()
    }
}

pub(crate) async fn patch_track_tags(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    Json(edits): Json<TagEdits>,
) -> Response {
    if edits.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "Request body has no recognized fields to update. Supported: title, artist, \
             album, album_artist, genre, year, track_number, disc_number.",
        )
            .into_response();
    }

    let track = {
        let tracks = state.tracks.read().await;
        match tracks.get(&id) {
            Some(t) => t.clone(),
            None => return StatusCode::NOT_FOUND.into_response(),
        }
    };

    let path = state.track_abs_path(&track);
    let db = state.library_db.clone();
    let root = track.root;
    let relative_path = track.relative_path.clone();
    let write_result = tokio::task::spawn_blocking(move || {
        write_tags(&path, &edits)?;
        let fingerprint = fingerprint_file(&path)?;
        db.resolve_guid(root, &relative_path, &fingerprint)?;
        Ok::<_, anyhow::Error>(edits)
    })
    .await;

    let edits = match write_result {
        Ok(Ok(edits)) => edits,
        Ok(Err(e)) => {
            tracing::error!("Failed writing tags for track {id}: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Couldn't write tags to the file. It may be read-only, in an unsupported \
                 format for writing, or in use by another program.",
            )
                .into_response();
        }
        Err(e) => {
            tracing::error!("Tag-write thread panicked for track {id}: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal error writing tags.",
            )
                .into_response();
        }
    };

    let updated = {
        let mut tracks = state.tracks.write().await;
        let Some(existing) = tracks.get_mut(&id) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        apply_edits_in_place(existing, &edits);
        existing.clone()
    };

    state.lyrics_cache.write().await.remove(&id);
    state.emit(LibraryEvent::TrackUpdated { track_id: id });

    Json(updated).into_response()
}

fn apply_edits_in_place(track: &mut Track, edits: &TagEdits) {
    if let Some(title) = &edits.title
        && !title.trim().is_empty()
    {
        track.title = title.clone();
    }
    if let Some(artist) = &edits.artist {
        track.artist = if artist.trim().is_empty() {
            "Unknown Artist".to_string()
        } else {
            artist.clone()
        };
    }
    if let Some(album) = &edits.album
        && !album.trim().is_empty()
    {
        track.album = album.clone();
    }
    if let Some(album_artist) = &edits.album_artist {
        track.album_artist = if album_artist.trim().is_empty() {
            None
        } else {
            Some(album_artist.clone())
        };
    }
    if let Some(genre) = &edits.genre {
        track.genre = if genre.trim().is_empty() {
            None
        } else {
            Some(genre.clone())
        };
    }
    if let Some(year) = edits.year {
        track.year = Some(year);
    }
    if let Some(track_number) = edits.track_number {
        track.track_number = Some(track_number);
    }
    if let Some(disc_number) = edits.disc_number {
        track.disc_number = Some(disc_number);
    }
}

fn write_tags(path: &Path, edits: &TagEdits) -> anyhow::Result<()> {
    let mut tagged_file = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Probe::open(path).and_then(|p| p.read())
    }))
    .map_err(|_| anyhow::anyhow!("tag parser panicked on this file (corrupt/weird file)"))??;

    if tagged_file.primary_tag().is_none() {
        let tag_type = tagged_file.file_type().primary_tag_type();
        tagged_file.insert_tag(Tag::new(tag_type));
    }
    let tag = tagged_file
        .primary_tag_mut()
        .ok_or_else(|| anyhow::anyhow!("file has no writable tag slot"))?;

    if let Some(title) = &edits.title {
        tag.set_title(title.clone());
    }
    if let Some(artist) = &edits.artist {
        tag.set_artist(artist.clone());
    }
    if let Some(album) = &edits.album {
        tag.set_album(album.clone());
    }
    if let Some(album_artist) = &edits.album_artist {
        if album_artist.trim().is_empty() {
            tag.remove_key(&ItemKey::AlbumArtist);
        } else {
            tag.insert_text(ItemKey::AlbumArtist, album_artist.clone());
        }
    }
    if let Some(genre) = &edits.genre {
        tag.set_genre(genre.clone());
    }
    if let Some(year) = edits.year {
        tag.set_year(year);
    }
    if let Some(track_number) = edits.track_number {
        tag.set_track(track_number);
    }
    if let Some(disc_number) = edits.disc_number {
        tag.set_disk(disc_number);
    }

    tagged_file
        .save_to_path(path, WriteOptions::default())
        .map_err(|e| anyhow::anyhow!("failed saving tags: {e}"))?;

    Ok(())
}
