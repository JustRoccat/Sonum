# Sonum API documentation

This file describes every endpoint the Sonum server offers: what it does, what parameters it takes, and what it returns.

## Authorization

If `api_token` is set in the config, every request must include this header:

```
Authorization: Bearer your-token
```

Without a valid token, the server returns `401 Unauthorized`. If `api_token` is not set, authorization is off and anyone can send requests.

## Multiple music folders

Sonum can scan more than one folder as a single combined library - see `music_dir`/`music_dirs` in the config file (README has the full syntax). Each configured folder gets a numeric **root index** (0, 1, 2, ... in config order). That index shows up in two places:

- Track ids are content-derived GUIDs (see "Track object" below), so the same relative path in two different folders never collides, and moving a file between roots keeps its id.
- `/files/:root/*path` serves files from a specific root - see below.

If you only configure one folder (the default), everything behaves exactly as before, just always under root `0`.

## Track object

Many endpoints return a track object in this format:

```json
{
  "id": "a1b2c3d4e5f60718",
  "title": "Track name",
  "artist": "Artist name",
  "album_artist": "Various Artists",
  "album": "Album name",
  "duration_seconds": 214,
  "bitrate_kbps": 320,
  "format": "mp3",
  "stream_url": "/tracks/a1b2c3d4e5f60718/stream",
  "has_embedded_lyrics": true,
  "has_embedded_art": true,
  "genre": "Rock",
  "year": 1994,
  "track_number": 3,
  "disc_number": 1,
  "added_at": 1732012345
}
```

The `id` field is a randomly generated, persistent GUID, stored in a small SQLite database (`library.sqlite`, next to `sonum.conf`) alongside a content fingerprint of the file. Unlike a path-derived id, it **survives renames, moves within a root, and moving the whole library folder somewhere else**: on every (re)scan, Sonum fingerprints each file and looks it up by content first, so a file that shows up at a new path with the same bytes keeps its old id instead of minting a new one. This is what keeps client-side playlists, favorites, and "resume position" data from silently breaking when you reorganize your files on disk.

If a file is genuinely deleted (its content doesn't reappear anywhere else in the library), its row is removed from `library.sqlite` too, so the database never accumulates stale entries.

If a file has no title tag, Sonum uses the file name as the title. If it has no artist tag, Sonum uses "Unknown Artist". If it has no album tag, Sonum uses the name of the folder the file is in.

`album_artist`, `genre`, `year`, `track_number`, and `disc_number` are omitted/`null` when the file has no corresponding tag - Sonum does not guess or fall back for these the way it does for title/artist/album.

`added_at` is a Unix timestamp (seconds) approximating when the file was added to the library: the file's birth time if the OS/filesystem exposes one, otherwise its last-modified time. It's preserved across rescans (including tag edits made through this API), so it won't jump to "now" just because the library got rescanned. It's a best-effort proxy, not an exact "date added to Sonum" log - Sonum keeps no persistent history across restarts.

## Endpoints

### `GET /tracks`

Returns a list of tracks. It accepts these optional query parameters:

| Parameter | Description |
|---|---|
| `q` | Search text. Sonum looks for it in the title, artist, and album, case insensitive. |
| `limit` | Maximum number of results per page. Capped at 2000 server-side regardless of what's requested. |
| `offset` | How many results to skip from the start. |
| `sort` | `artist` (default): by artist, then title. `title`: by title. `added_desc`: most recently added first. `added_asc`: oldest first. Unrecognized values fall back to the default. |

The response is an array of track objects. Response headers carry extra information:

- `X-Total-Count`: the total number of matching tracks, before `limit` and `offset` are applied.
- `ETag`: an identifier for the current state of the library.
- `Last-Modified`: the date of the last scan.

If you send an `If-None-Match` header that matches the current `ETag`, the server replies `304 Not Modified` with no body. This lets a client skip downloading the full list when nothing has changed.

**Example - "recently added" view:**

```
GET /tracks?sort=added_desc&limit=50
```

### `GET /tracks/:id`

Returns a single track by `id`. If the track does not exist, the server returns `404 Not Found`.

### `PATCH /tracks/:id`

Edits a track's tags and writes them back to the audio file on disk. Send a JSON body with any subset of these fields - only the fields present are changed:

```json
{
  "title": "New Title",
  "artist": "New Artist",
  "album": "New Album",
  "album_artist": "Various Artists",
  "genre": "Electronic",
  "year": 2024,
  "track_number": 5,
  "disc_number": 1
}
```

An empty string for `artist`, `album_artist`, or `genre` clears that field (album_artist/genre become unset; artist resets to "Unknown Artist"). Empty `title`/`album` are ignored rather than clearing, since those always need a display value.

On success, returns the updated track object (`200 OK`). On failure: `404 Not Found` if the track doesn't exist, `400 Bad Request` if the body has no recognized fields, or `500 Internal Server Error` if the file couldn't be written (read-only file, unsupported format for writing, file in use elsewhere, etc).

> [!NOTE]
> This writes directly to your audio files. Keep backups. Sonum does not keep an undo history.

Editing tags updates Sonum's in-memory index immediately (no rescan needed) and clears that track's cached lyrics lookup. It also emits a `track_updated` event on `/events` (see below).

### `GET /tracks/:id/stream`

By default, redirects (`307 Temporary Redirect`) to the audio file under `/files/:root/...`, where the file is served directly, unmodified. If `id` does not exist, the server returns `404 Not Found`.

Optionally transcodes on the fly instead of redirecting, for clients with limited support for the source format:

| Parameter | Description |
|---|---|
| `format` | `mp3`, `opus`, or `aac`. Transcodes the track into this format via `ffmpeg` and streams the result. |
| `bitrate` | Target bitrate in kbps for the transcode, clamped to 64-320. Defaults to 192. Ignored unless `format` is set. |

If `ffmpeg` isn't available on the server, a `format` request returns `501 Not Implemented`.

**Seeking a transcoded stream:** send a `Range: bytes=<start>-` header, same as you would against a static file. Sonum translates the requested starting byte into an approximate time offset (using the target bitrate) and has `ffmpeg` start encoding from there, replying `206 Partial Content` with `Content-Range: bytes <start>-*/*` (the total length is unknown ahead of a fresh encode, which RFC 7233 allows expressing as `*`). This is a best-effort approximation, not a byte-exact seek - variable-bitrate source material or encoder overhead can make the actual resume point drift by a bit. If you need exact seeking, don't transcode: the no-`format` path redirects to the original file, which supports normal byte-accurate range requests.

### `GET /tracks/:id/lyrics`

Returns lyrics for a track. Sonum looks for lyrics in this order:

1. Lyrics stored in the audio file's tags.
2. A `.lrc` file with the same name as the audio file, in the same folder.
3. A shared `.lrc` file in the folder (for example `lyrics.lrc`, or a file named after the folder), but only when the folder contains exactly one audio file.

If nothing is found, the response is `200 OK` with an empty result, not an error.

The result is cached, so later requests are fast. The cache clears automatically after the library is rescanned, and for a single track right after its tags are edited via `PATCH /tracks/:id`.

Response format:

```json
{
  "track_id": "a1b2c3d4e5f60718",
  "source": "track_lrc",
  "synced": true,
  "lines": [
    { "time_ms": 12500, "text": "First line of lyrics" },
    { "time_ms": 16200, "text": "Second line of lyrics" }
  ],
  "text": null
}
```

The `source` field is one of: `embedded`, `track_lrc`, `shared_lrc`, or `none`.

If the lyrics have timestamps (LRC format), `synced` is `true` and the timed lines are in `lines`. If they do not have timestamps, `synced` is `false`, `lines` is empty, and the full text is in `text`.

### `GET /tracks/:id/art` and `GET /art/:id`

Returns album art as an image, not as JSON. Sonum looks for art in this order:

1. An image stored in the audio file's tags.
2. A file in the same folder named `cover.jpg`, `folder.png`, `front.webp`, or similar.
3. A file with the same name as the track, with extension `.jpg`, `.jpeg`, `.png`, or `.webp`.

Query parameters:

| Parameter | Description |
|---|---|
| `placeholder` | Default `true`. When `true` and no art is found, returns a placeholder SVG instead of `404`. |
| `size` | `full` (default): the original image, as found, unmodified. `thumbnail` (or `thumb`): resized to fit within 300x300px and re-encoded as JPEG, regardless of the source format. |

The `X-Art-Source` response header shows where the image came from: `embedded`, `folder`, `track_named`, or `placeholder`.

The result is cached per `(track, size)`, the same way lyrics are cached per track.

### `GET /albums`

Returns tracks grouped into albums, so clients don't each have to implement this grouping themselves. Grouping key is `(album name, grouping artist)`, where the grouping artist is the shared **album artist** tag if the tracks have one, otherwise the (single) track artist - this keeps compilations/various-artists albums from splitting into one entry per featured artist.

Accepts the same `q`, `limit`, and `offset` query parameters as `/tracks` (searching `q` against album name and artist). Response headers include `X-Total-Count`, same meaning as on `/tracks`.

Results are sorted by artist, then by album name.

```json
[
  {
    "name": "Now That's What I Call Music!",
    "artist": "Various Artists",
    "is_compilation": true,
    "year": 2005,
    "track_count": 18,
    "track_ids": ["a1b2c3d4e5f60718", "b2c3d4e5f6071829"],
    "art_url": "/art/a1b2c3d4e5f60718"
  }
]
```

`year` is the year of the first track in the album that has one set; it's `null` if none do. `art_url` points at one of the album's tracks, as a best-effort cover - Sonum has no separate concept of album-level art. `is_compilation` is `true` when the album's tracks don't all share the same individual artist (i.e. it only stayed grouped together because of a shared album-artist tag).

### `GET /artists`

Same idea as `/albums`, grouped by each track's own artist (not album artist - a guest vocalist still gets their own artist page):

```json
[
  {
    "name": "The Beatles",
    "album_count": 12,
    "track_count": 213,
    "track_ids": ["a1b2c3d4e5f60718", "..."]
  }
]
```

Accepts the same `q` (matched against artist name), `limit`, and `offset` parameters, and the same `X-Total-Count` header, as `/albums`. Results are sorted by name.

### `GET /duplicates`

Returns groups of tracks that look like duplicates of each other, using a heuristic: tracks whose title and artist normalize to the same text (case/punctuation-insensitive) and whose durations are within 3 seconds of each other are grouped together.

```json
[
  {
    "reason": "same title/artist, similar duration",
    "tracks": [ /* full track objects */ ]
  }
]
```

This is a heuristic, not a guarantee: it does not hash or compare audio content, so it won't catch duplicates with different tags, and it can occasionally group two genuinely different short recordings that happen to share a title and artist. Treat it as a starting point for manual review, not an automatic delete list.

### `GET /tree`

Returns the folder structure of the music library as a tree. With multiple configured `music_dir`s, each one gets its own top-level node (named after its folder, config order preserved), so folders from different libraries never merge even if they share subfolder names. Each node has a name, a list of children (subfolders), and a list of track IDs found directly in that folder.

```json
{
  "name": "root",
  "children": [
    {
      "name": "Music",
      "children": [
        {
          "name": "Beatles",
          "children": [],
          "track_ids": ["a1b2c3d4e5f60718", "b2c3d4e5f6071829"]
        }
      ],
      "track_ids": []
    }
  ]
}
```

### `GET /events`

Server-Sent Events stream of library changes, so a client can react live instead of polling `/tracks` with `If-None-Match`. Each event is a JSON object on its own `data:` line:

```json
{ "type": "rescanned", "tracks_total": 1523, "duration_ms": 812 }
{ "type": "track_updated", "track_id": "a1b2c3d4e5f60718" }
```

`rescanned` fires after both full (`POST /rescan`, startup) and incremental (file watcher) scans. `track_updated` fires after a successful `PATCH /tracks/:id`. The connection sends a `keep-alive` comment every 15 seconds so proxies/load balancers don't time it out as idle.

Browser example:

```js
const es = new EventSource("/events");
es.onmessage = (e) => console.log(JSON.parse(e.data));
```

### `POST /rescan`

Triggers a new scan of every configured music folder and returns the number of tracks found. Useful if you do not want to wait for automatic change detection, or the file watcher did not trigger (for example, on some network file systems).

```json
{ "rescanned": true, "tracks_found": 1523 }
```

> [!NOTE]
> A rescan clears the lyrics and art caches. The first request after a scan may be a bit slower.

### `GET /health`

A simple endpoint to check whether the server is running. Useful for monitoring and health checks.

```json
{ "status": "ok", "uptime_seconds": 3600, "tracks_indexed": 1523, "music_dirs": 2 }
```

### `GET /metrics`

Returns server metrics in a format compatible with Prometheus. This endpoint is meant for a monitoring system, not for manual browsing.

Available metrics:

| Name | Type | Description |
|---|---|---|
| `sonum_uptime_seconds` | counter | How long the server has been running, in seconds. |
| `sonum_tracks_indexed` | gauge | Number of tracks currently indexed. |
| `sonum_requests_total` | counter | Total number of HTTP requests handled. |
| `sonum_memory_rss_bytes` | gauge | RSS memory usage in bytes. Available on Linux only. |
| `sonum_last_scan_duration_ms` | gauge | How long the last library scan took, in milliseconds. |

### `GET /files/:root/...`

Direct access to files inside music folder number `:root` (0-indexed, in config order - see "Multiple music folders" above). This endpoint handles streaming (`GET /tracks/:id/stream` redirects here). Paths are percent-encoded, so special characters in file and folder names work correctly. Supports normal HTTP range requests (unlike the transcoded-stream path), so seeking/scrubbing on the original file is exact.

**Backward compatibility:** with exactly one `music_dir` configured (the common case), files are *also* served at the old, non-prefixed `/files/...` path with no root index - so a client that hardcoded file URLs against an older version of Sonum keeps working unchanged. This alias only exists for the single-folder case; once you configure more than one `music_dir`, only the `/files/:root/...` form is available, since there's no single correct folder for a bare `/files/...` to mean anymore.

## Background scanning

Sonum watches every configured music folder at all times. When it detects a change (a new file, a deleted file, an edit), it waits half a second to gather any further changes into one batch, then updates only the affected files/folders in the in-memory index (an incremental rescan), rather than re-walking every folder. This keeps large libraries responsive to change without a full rescan on every edit.

`POST /rescan` still does a full walk of every music folder, since it's meant as an explicit "start over" for cases the watcher might miss (e.g. some network filesystems don't emit change events Sonum can see). To avoid someone accidentally (or deliberately) hammering the most expensive endpoint in a loop, a manual `/rescan` request sooner than 10 seconds after the previous scan finished (including the automatic scan done at startup) gets `429 Too Many Requests` with a `Retry-After` header.

## Rate limiting

Every endpoint is rate-limited per client IP, using the `rate_limit_per_min` config setting (default 300 requests/minute; set to `0` to disable). Requests over the limit get `429 Too Many Requests` with a `Retry-After` header indicating how many seconds to wait. This is a simple fixed-window limiter meant to blunt obvious abuse, not to enforce precise quotas - it is not a substitute for putting the server behind a reverse proxy if you expose it beyond your own network.

## Compression and CORS

The server compresses responses by default, when the client supports it, and allows requests from any origin (CORS is open to all). If you need a stricter CORS policy, change the settings in `main.rs`.

## HTTPS

Sonum can terminate TLS itself instead of (or as well as) sitting behind a reverse proxy: set both `tls_cert_path` and `tls_key_path` in the config to PEM file paths. When both are set, the server listens over HTTPS on `bind_addr` instead of plain HTTP. Setting only one of the two is a config error and the server refuses to start.
