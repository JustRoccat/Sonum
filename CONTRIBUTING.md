# Contributing to Sonum

Thanks for your interest in improving Sonum. This guide covers how to set up the project, the general code style, and how to submit changes.

## Getting started

You need Rust installed, ideally the latest stable version. Clone the repository and build it:

```bash
git clone <repository url>
cd sonum
cargo build
```

Run the server locally while you work:

```bash
RUST_LOG=sonum=debug cargo run
```

Point `music_dir` in `~/.config/sonum/sonum.conf` at a small test folder with a few audio files, so scans stay fast while you develop.

## Project layout

Each file in the project has one clear job:

| File | Responsibility |
|---|---|
| `main.rs` | Server setup, routes, middleware |
| `config.rs` | Reading and writing the config file |
| `state.rs` | Shared application state and the `Track` type |
| `handlers.rs` | Core endpoints: list, get, stream, health, rescan, auth |
| `scan.rs` | Scanning the music folder and reading tags |
| `lyrics.rs` | Finding and parsing lyrics |
| `art.rs` | Finding album art |
| `tree.rs` | Building the folder tree |
| `metrics.rs` | Prometheus metrics |
| `util.rs` | Small helper functions |

If you add a feature that does not fit any of these, it is fine to add a new module instead of stretching an existing one.

## Before you open a pull request

1. Run `cargo fmt` so formatting stays consistent across the codebase.
2. Run `cargo clippy` and fix warnings that relate to your change.
3. Run `cargo build --release` at least once to catch anything the dev profile hides.
4. Test your change against a real music folder, not just an empty one. Metadata handling has a lot of edge cases: missing tags, unusual characters in file names, folders with mixed formats.

> [!TIP]
> If your change touches `scan.rs`, `lyrics.rs`, or `art.rs`, test it against files with missing or broken tags too. These modules are built to degrade gracefully, and a good change should keep that behavior.

## Code style notes

- Keep new endpoints thin. Put the actual logic in a helper function so it is easy to test and reuse.
- Prefer returning `Option` and `Result` over panicking. Tag parsing already wraps risky calls in `catch_unwind`, because some files trigger panics deep in third-party parsers. Follow that pattern for similar risky code.
- Keep blocking file I/O inside `tokio::task::spawn_blocking`, the same way `scan.rs`, `lyrics.rs`, and `art.rs` do it. Do not call blocking `fs` functions directly inside an async handler.
- Match the existing error handling style: log a warning and skip the problem file, instead of failing the whole scan.

## Reporting bugs

Open an issue with:

- What you expected to happen.
- What happened instead.
- Your OS and Rust version (`rustc --version`).
- If it is related to a specific file, the format and, if possible, a way to reproduce it (a sample file with similar tags, if you can share one).

## Submitting a pull request

1. Fork the repository and create a branch for your change.
2. Keep the pull request focused on one change. Smaller PRs are easier to review and merge.
3. Describe what the change does and why, not just what files it touches.
4. Link any related issue.

If you are not sure whether a change fits the project, open an issue first to discuss it before writing code.
