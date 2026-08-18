# Sonum API documentation

This file describes every endpoint the Sonum server offers: what it does, what parameters it takes, and what it returns.

## Authorization

If `api_token` is set in the config, every request must include this header:

```
Authorization: Bearer your-token
```

Without a valid token, the server returns `401 Unauthorized`. If `api_token` is not set, authorization is off and anyone can send requests.

## Track object

Many endpoints return a track object in this format:

```json
{
  "id": "a1b2c3d4e5f60718",
  "title": "Track name",
  "artist": "Artist name",
  "album": "Album name",
  "duration_seconds": 214,
  "bitrate_kbps": 320,
  "format": "mp3",
  "stream_url": "/tracks/a1b2c3d4e5f60718/stream",
  "has_embedded_lyrics": true,
  "has_embedded_art": true
}
```

The `id` field is a SHA-256 hash of the file path (the first 8 bytes, written as hex). It stays the same as long as the file path does not change.

If a file has no title tag, Sonum uses the file name as the title. If it has no artist tag, Sonum uses "Unknown Artist". If it has no album tag, Sonum uses the name of the folder the file is in.

## Endpoints

### `GET /tracks`

Returns a list of tracks. It accepts three optional query parameters:

| Parameter | Description |
|---|---|
| `q` | Search text. Sonum looks for it in the title, artist, and album, case insensitive. |
| `limit` | Maximum number of results per page. |
| `offset` | How many results to skip from the start. |

Results are sorted by artist first, then by title.

The response is an array of track objects. Response headers carry extra information:

- `X-Total-Count`: the total number of matching tracks, before `limit` and `offset` are applied.
- `ETag`: an identifier for the current state of the library.
- `Last-Modified`: the date of the last scan.

If you send an `If-None-Match` header that matches the current `ETag`, the server replies `304 Not Modified` with no body. This lets a client skip downloading the full list when nothing has changed.

**Example:**

```
GET /tracks?q=beatles&limit=20&offset=0
```

### `GET /tracks/:id`

Returns a single track by `id`. If the track does not exist, the server returns `404 Not Found`.

### `GET /tracks/:id/stream`

Redirects (`307 Temporary Redirect`) to the audio file under `/files/...`, where the file is served directly. If `id` does not exist, the server returns `404 Not Found`.

### `GET /tracks/:id/lyrics`

Returns lyrics for a track. Sonum looks for lyrics in this order:

1. Lyrics stored in the audio file's tags.
2. A `.lrc` file with the same name as the audio file, in the same folder.
3. A shared `.lrc` file in the folder (for example `lyrics.lrc`, or a file named after the folder), but only when the folder contains exactly one audio file.

If nothing is found, the response is `200 OK` with an empty result, not an error.

The result is cached, so later requests are fast. The cache clears automatically after the library is rescanned.

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

The `placeholder` query parameter (default `true`) controls what happens when nothing is found:

- `placeholder=true` (default): the server returns a simple placeholder image (SVG).
- `placeholder=false`: the server returns `404 Not Found`.

The `X-Art-Source` response header shows where the image came from: `embedded`, `folder`, `track_named`, or `placeholder`.

The result is cached, the same way lyrics are.

### `GET /tree`

Returns the folder structure of the music library as a tree. Each node has a name, a list of children (subfolders), and a list of track IDs found directly in that folder.

```json
{
  "name": "root",
  "children": [
    {
      "name": "Beatles",
      "children": [],
      "track_ids": ["a1b2c3d4e5f60718", "b2c3d4e5f6071829"]
    }
  ]
}
```

### `POST /rescan`

Triggers a new scan of the music folder and returns the number of tracks found. Useful if you do not want to wait for automatic change detection, or the file watcher did not trigger (for example, on some network file systems).

```json
{ "rescanned": true, "tracks_found": 1523 }
```

> [!NOTE]
> A rescan clears the lyrics and art caches. The first request after a scan may be a bit slower.

### `GET /health`

A simple endpoint to check whether the server is running. Useful for monitoring and health checks.

```json
{ "status": "ok", "uptime_seconds": 3600, "tracks_indexed": 1523 }
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

### `GET /files/...`

Direct access to files inside the `music_dir` folder. This endpoint handles streaming (`GET /tracks/:id/stream` redirects here). Paths are percent-encoded, so special characters in file and folder names work correctly.

## Background scanning

Sonum watches the `music_dir` folder at all times. When it detects a change (a new file, a deleted file, an edit), it waits half a second to gather any further changes into one batch, then runs a full rescan. This keeps the library up to date without any manual refresh.

## Compression and CORS

The server compresses responses by default, when the client supports it, and allows requests from any origin (CORS is open to all). If you need a stricter CORS policy, change the settings in `main.rs`.
