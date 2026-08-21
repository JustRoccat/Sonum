# Sonum

Sonum is a simple music server you can run on your own computer or server. It scans a music folder you choose and makes it available through a plain HTTP API. You can browse your tracks, fetch lyrics and album art, and stream audio files to any client app.

## Why use it

- It runs locally, so your music stays on your own hardware.
- It can scan more than one folder as a single combined library (e.g. a local folder and a NAS mount).
- It detects title, artist, album, album artist, duration, and bitrate for each file automatically, and groups compilations/various-artists albums correctly using the album-artist tag.
- It lets you edit a track's tags through the API, writing the change straight back to the file.
- It can flag likely duplicate tracks across your library.
- It finds lyrics on its own, either from file tags or from `.lrc` files.
- It finds album art on its own, from tags, from a folder image (like `cover.jpg`), or from a separate file next to the track - full size or as a small thumbnail.
- It watches every configured music folder for changes and refreshes the library on its own (incrementally, not a full rescan every time), no manual restart needed.
- It streams live library-change notifications over Server-Sent Events, so clients don't have to poll.
- It supports the most common formats: MP3, FLAC, OGG, OPUS, M4A, WAV, and AAC.
- It exposes albums and artists as pre-grouped, paginated endpoints, so clients don't each have to reimplement grouping.
- It can transcode on the fly to MP3, Opus, or AAC (if `ffmpeg` is installed) for clients with limited format support, with best-effort seeking support via HTTP Range requests.
- It rate-limits requests per IP to blunt accidental or intentional abuse (e.g. a client hammering `/rescan` in a loop).
- It can terminate HTTPS itself, if you'd rather not put a reverse proxy in front of it.

Sonum intentionally has **no built-in concept of playlists, play counts, or user accounts** - it's a metadata/streaming API, and anything about how a listener organizes or tracks their own listening is left entirely up to whatever client you build against it.

## Installation

You need Rust installed - **1.85 or newer** (the project uses the 2024 edition).

```bash
git clone <repository url>
cd sonum
cargo build --release
```

Start the server with:

```bash
cargo run --release
```

On first run, the program creates a config file at `~/.config/sonum/sonum.conf` and a default music folder. Drop your files there and restart the server, or just wait, since the watcher picks up new files on its own.

## Configuration

The config file lives at `~/.config/sonum/sonum.conf`. It has these settings:

| Key | Description | Default |
|---|---|---|
| `music_dir` | Music folder. Sonum scans it and all subfolders. Repeat this line to add more than one folder. | `~/.config/sonum/music` |
| `music_dirs` | Alternative to repeating `music_dir`: a single comma-separated line, e.g. `music_dirs = /a, /b`. Both forms can be combined. | none |
| `bind_addr` | Address and port the server listens on. | `127.0.0.1:8420` |
| `api_token` | Token used to authorize requests. Optional. | none |
| `rate_limit_per_min` | Max requests per client IP per minute, across all endpoints. Set to `0` to disable. | `300` |
| `tls_cert_path` | PEM certificate file path, to serve HTTPS directly. Must be set together with `tls_key_path`. | none |
| `tls_key_path` | PEM private key file path, to serve HTTPS directly. Must be set together with `tls_cert_path`. | none |

**Multiple music folders:**

```
music_dir = /home/yourname/Music
music_dir = /mnt/nas/MoreMusic
```

or equivalently:

```
music_dirs = /home/yourname/Music, /mnt/nas/MoreMusic
```

Each folder is scanned as part of one combined library. Tracks, albums, and artists from different folders are merged together in `/tracks`, `/albums`, and `/artists`; `/tree` shows each folder as its own top-level branch, and `/files/:root/...` addresses a specific folder by its position in the config (0, 1, 2, ...) - see [DOCS.md](DOCS.md) for details.

> [!WARNING]
> If you do not set `api_token`, the server runs with no authorization at all. This is fine only when the server listens on `localhost`. Do not expose a server like this to the internet.

> [!NOTE]
> You must restart the server after any change to the config file. Sonum does not reload config changes automatically, unlike changes to the music files themselves.

If you set `api_token`, every request to the server must include this header:

```
Authorization: Bearer your-token
```

**Built-in HTTPS:** set both `tls_cert_path` and `tls_key_path` to serve HTTPS directly instead of (or as well as) putting a reverse proxy in front of Sonum:

```
tls_cert_path = /etc/sonum/fullchain.pem
tls_key_path = /etc/sonum/privkey.pem
```

Setting only one of the two is a config error and the server refuses to start.

## API documentation

You can find the full list of endpoints, response formats, and error codes in a separate file: [DOCS.md](DOCS.md).

## Development

Sonum is written in Rust and built on the [axum](https://github.com/tokio-rs/axum) framework. It reads audio tags using the [lofty](https://github.com/Serial-ATA/lofty-rs) library.

To run the project locally for testing:

```bash
cargo run
```

Control the log level with the `RUST_LOG` environment variable. Example:

```bash
RUST_LOG=sonum=debug cargo run
```

> [!TIP]
> Want to add a new endpoint? Add a handler function in `handlers.rs` (or in a new file, if it belongs to a new feature) and register the route in `main.rs`.

Report bugs and suggest changes through issues or pull requests in the repository.

### Known portability issues

- SIGTERM handling only works on Unix systems (Linux, macOS). On Windows, the server only reacts to Ctrl+C.
- The memory usage metric (`sonum_memory_rss_bytes` in `/metrics`) only works on Linux. It reads from `/proc/self/status`, which does not exist on other systems.
- On-the-fly transcoding (`?format=...` on the stream endpoint) requires an `ffmpeg` binary on `PATH`. If it's missing, the server still runs fine, it just answers those requests with `501 Not Implemented` and streams the original file when `format` isn't requested at all.

## File layout

- `main.rs`: entry point of the program. Sets up the server, API routes, and middleware.
- `config.rs`: loads and creates the config file.
- `state.rs`: holds shared application state, such as the track list, settings, and caches.
- `handlers.rs`: handles the main endpoints, such as listing tracks, fetching a single track, streaming, and authorization.
- `scan.rs`: scans every configured music folder, reads metadata from audio files, and applies incremental updates from the file watcher.
- `tags.rs`: writes tag edits back to audio files for `PATCH /tracks/:id`.
- `duplicates.rs`: heuristic duplicate-track detection for `/duplicates`.
- `events.rs`: Server-Sent Events stream of library changes for `/events`.
- `groups.rs`: groups tracks into albums and artists for the `/albums` and `/artists` endpoints, including album-artist/compilation handling.
- `transcode.rs`: on-the-fly audio transcoding via `ffmpeg`, plus best-effort seek support for transcoded streams.
- `ratelimit.rs`: per-IP request rate limiting.
- `lyrics.rs`: finds and parses song lyrics.
- `art.rs`: finds album art and generates thumbnails.
- `tree.rs`: builds the folder structure as a tree.
- `metrics.rs`: exposes server metrics in Prometheus format.
- `util.rs`: small helper functions, such as generating a track ID.
