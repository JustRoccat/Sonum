# Sonum

Sonum is a simple music server you can run on your own computer or server. It scans a music folder you choose and makes it available through a plain HTTP API. You can browse your tracks, fetch lyrics and album art, and stream audio files to any client app.

## Why use it

- It runs locally, so your music stays on your own hardware.
- It detects title, artist, album, duration, and bitrate for each file automatically.
- It finds lyrics on its own, either from file tags or from `.lrc` files.
- It finds album art on its own, from tags, from a folder image (like `cover.jpg`), or from a separate file next to the track.
- It watches the music folder for changes and refreshes the library on its own, no manual restart needed.
- It supports the most common formats: MP3, FLAC, OGG, OPUS, M4A, WAV, and AAC.

## Installation

You need Rust installed, ideally the latest stable version.

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

The config file lives at `~/.config/sonum/sonum.conf`. It has three settings:

| Key | Description | Default |
|---|---|---|
| `music_dir` | Music folder. Sonum scans it and all subfolders. | `~/.config/sonum/music` |
| `bind_addr` | Address and port the server listens on. | `127.0.0.1:8420` |
| `api_token` | Token used to authorize requests. Optional. | none |

> [!WARNING]
> If you do not set `api_token`, the server runs with no authorization at all. This is fine only when the server listens on `localhost`. Do not expose a server like this to the internet.

> [!NOTE]
> You must restart the server after any change to the config file. Sonum does not reload config changes automatically, unlike changes to the music files themselves.

If you set `api_token`, every request to the server must include this header:

```
Authorization: Bearer your-token
```

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

## File layout

- `main.rs`: entry point of the program. Sets up the server, API routes, and middleware.
- `config.rs`: loads and creates the config file.
- `state.rs`: holds shared application state, such as the track list, settings, and caches.
- `handlers.rs`: handles the main endpoints, such as listing tracks, fetching a single track, streaming, and authorization.
- `scan.rs`: scans the music folder and reads metadata from audio files.
- `lyrics.rs`: finds and parses song lyrics.
- `art.rs`: finds album art.
- `tree.rs`: builds the folder structure as a tree.
- `metrics.rs`: exposes server metrics in Prometheus format.
- `util.rs`: small helper functions, such as generating a track ID.
