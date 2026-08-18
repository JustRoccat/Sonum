# Security policy

## Reporting a vulnerability

If you find a security issue in Sonum, please do not open a public issue. Instead, report it privately, for example through GitHub's private vulnerability reporting feature on this repository, or by email to the maintainer.

Please include:

- A description of the issue and its impact.
- Steps to reproduce it, or a proof of concept if you have one.
- The version or commit you tested against.

You should get a response within a few days. Once a fix is ready, it will be released and the issue can be disclosed publicly.

## Security notes for users

Sonum is built to run on a trusted local network or on `localhost`, not as a public-facing service. Keep these points in mind:

> [!WARNING]
> Without `api_token` set in the config, the server accepts requests from anyone who can reach it, with no login and no rate limiting. Only run it without a token when it listens on `127.0.0.1` or another address you fully control.

- **CORS is open to all origins by default.** This is convenient for local apps and browser-based clients, but it means any website a user visits in their browser can call your Sonum server if it can reach it over the network. Keep the server on a private network if this matters to you.
- **The `/files` route serves the whole `music_dir` folder directly.** Do not point `music_dir` at a folder that contains files you do not want exposed over HTTP.
- **`api_token` is stored in plain text** in `~/.config/sonum/sonum.conf`. Set file permissions on that file so only your user account can read it, and never commit it to a repository.
- **There is no rate limiting or brute-force protection** on the `api_token` check. If you expose Sonum beyond `localhost`, put it behind a reverse proxy that adds TLS and, ideally, its own access control.
- **The file watcher and rescan endpoint follow symlinks and normal file system behavior.** Do not point `music_dir` at a location where untrusted users can drop arbitrary files, since anything with a matching audio extension gets scanned and, if tag parsing panics, only the individual file is skipped, not the whole scan.
