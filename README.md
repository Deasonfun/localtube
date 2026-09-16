# LocalTube

LocalTube is a local Rust/Axum video playback backend with a small WebExtension bridge. The backend accepts a safe video identifier, acquires media from an explicitly authorized local library, stores a temporary session copy, serves it with HTTP range support, and cleans it up.

This phase intentionally does **not** download arbitrary internet videos. A URL or API key does not imply permission to download media. The swappable `MediaProvider` trait allows another authorized source to be added later.

## Requirements

- Rust stable (edition 2024)
- `ffmpeg` and `ffprobe` available on `PATH`
- Media you are authorized to process

## Installation and running

Place a supported source file in `media/`. Its filename (without extension) is the video ID. Supported source extensions are `.mp4`, `.webm`, `.mkv`, and `.mov`.

```bash
cp /path/to/authorized-demo.mp4 media/demo.mp4
cargo run
```

Then open:

```text
http://127.0.0.1:6969/watch?v=demo
```

The provider uses `ffprobe` for metadata and `ffmpeg` to remux the source into the session's `video.mp4`. Because arguments are passed directly to child processes and IDs allow only ASCII letters, digits, `_`, and `-`, requests cannot inject shell commands or select arbitrary paths.

## Configuration

| Variable | Default | Meaning |
|---|---:|---|
| `LOCALTUBE_HOST` | `127.0.0.1` | Bind IP. Use a non-loopback address only intentionally. |
| `LOCALTUBE_PORT` | `6969` | HTTP port |
| `LOCALTUBE_CACHE_DIR` | `cache` | Temporary session directory |
| `LOCALTUBE_MEDIA_DIR` | `media` | Authorized local source library |
| `LOCALTUBE_SESSION_TIMEOUT` | `1800` | Inactive session timeout in seconds |
| `LOCALTUBE_CLEANUP_INTERVAL` | min(timeout, 300) | Cleanup scan interval in seconds |
| `LOCALTUBE_MAX_CONCURRENT_DOWNLOADS` | `2` | Maximum concurrent acquisition jobs |
| `LOCALTUBE_MEDIA_PROVIDER` | `local` | `local`, `http-fixture`, or `yt-dlp` |
| `LOCALTUBE_FIXTURE_BASE_URL` | `http://127.0.0.1:6970` | Trusted HTTP fixture server base URL |
| `LOCALTUBE_YTDLP_PATH` | `yt-dlp` | Path to yt-dlp executable |
| `LOCALTUBE_YOUTUBE_BASE_URL` | `https://www.youtube.com/watch?v={video_id}` | Trusted source template with `{video_id}` placeholder |
| `LOCALTUBE_YTDLP_COOKIES_FROM_BROWSER` | unset | Optional yt-dlp `--cookies-from-browser` value (e.g. `firefox`) |
| `LOCALTUBE_YTDLP_COOKIES_FILE` | unset | Optional yt-dlp `--cookies` file path |
| `LOCALTUBE_YTDLP_JS_RUNTIME` | `node` | yt-dlp JavaScript runtime (`node` or `node:/absolute/path`) |
| `LOCALTUBE_ACQUISITION_TIMEOUT` | session timeout | Maximum acquisition duration in seconds |
| `LOCALTUBE_FIXTURE_CONNECT_TIMEOUT` | `3` | Fixture connection timeout in seconds |
| `LOCALTUBE_ALLOWED_ORIGINS` | empty | Additional exact CORS origins, separated by commas |

Example:

```bash
LOCALTUBE_CACHE_DIR=/tmp/localtube LOCALTUBE_SESSION_TIMEOUT=600 cargo run
```

## Media providers

### Local provider

The default `LocalFileProvider` reads only supported files under `LOCALTUBE_MEDIA_DIR` and remains the deterministic offline fallback:

```bash
cp /path/to/authorized-video.mp4 media/demo.mp4
LOCALTUBE_MEDIA_PROVIDER=local cargo run --bin localtube
```

### HTTP fixture provider

`HttpFixtureProvider` proves external HTTP acquisition without accessing the public internet or making source-specific assumptions. Start the deterministic fixture server in one terminal:

```bash
cargo run --bin fixture_server
```

It generates a five-second browser-compatible test video with FFmpeg and serves:

```text
GET http://127.0.0.1:6970/metadata/demo
GET http://127.0.0.1:6970/media/demo
```

Start LocalTube in another terminal:

```bash
LOCALTUBE_MEDIA_PROVIDER=http-fixture \
LOCALTUBE_FIXTURE_BASE_URL=http://127.0.0.1:6970 \
cargo run --bin localtube
```

Then open `http://127.0.0.1:6969/watch?v=demo`. The page displays **LocalTube Demo Video**.

The base URL comes only from trusted server configuration. `/watch` accepts only validated video IDs and cannot select a URL or host. Redirects from the fixture server are disabled. Responses are streamed to the current session file, bounded by connection/request/acquisition timeouts, validated with FFprobe, and removed on failure. No credentials are used or logged.

### yt-dlp provider (authorized use only)

`YtDlpProvider` runs a configured `yt-dlp` executable and supports metadata and media acquisition for sources where downloading is explicitly authorized.

```bash
LOCALTUBE_MEDIA_PROVIDER=yt-dlp \
LOCALTUBE_YTDLP_PATH=yt-dlp \
LOCALTUBE_YOUTUBE_BASE_URL='https://www.youtube.com/watch?v={video_id}' \
LOCALTUBE_YTDLP_COOKIES_FROM_BROWSER=firefox \
LOCALTUBE_YTDLP_JS_RUNTIME=node \
cargo run --bin localtube
```

Security boundaries:

- `/watch` accepts only validated IDs.
- LocalTube constructs the source URL from trusted configuration and `{video_id}`.
- Arbitrary request URLs are not accepted.
- `yt-dlp` is invoked without a shell using explicit args.
- Optional auth context can be provided by exactly one of `LOCALTUBE_YTDLP_COOKIES_FROM_BROWSER` or `LOCALTUBE_YTDLP_COOKIES_FILE`.
- Download timeout and global acquisition concurrency limits still apply.
- Partial output artifacts are removed on failure.
- Acquired media is validated with FFprobe before `Ready`.

`yt-dlp`, Node.js, and `ffmpeg` must already be installed. LocalTube does not install executables at runtime. Verify Node with `node --version`; if it is outside `PATH`, set `LOCALTUBE_YTDLP_JS_RUNTIME=node:/absolute/path/to/node`.

## Usage

1. Start LocalTube with `cargo run --bin localtube`.
2. Open `http://127.0.0.1:6969/watch?v=<VIDEO_ID>`.
3. Wait while the authorized media is prepared.
4. Watch it in the native HTML5 player.
5. The session media is deleted when playback ends or after inactivity.

`GET /health` returns `{"status":"ok"}`.

The backend permits CORS `GET` requests from browser-extension origins using Firefox's `moz-extension://` and Chromium's `chrome-extension://` schemes. Temporary development extension IDs vary, so these extension-only schemes are recognized without allowing arbitrary web origins. Additional exact origins can be supplied with `LOCALTUBE_ALLOWED_ORIGINS`, for example:

```bash
LOCALTUBE_ALLOWED_ORIGINS=https://trusted.example,http://localhost:3000 cargo run
```

No wildcard `Access-Control-Allow-Origin` policy is used.

## Browser extension

The Phase 2 extension in `extension/` recognizes supported YouTube `/watch?v=...` navigation, checks the backend health endpoint, and redirects to the existing LocalTube `/watch` endpoint. It does not acquire or process media.

See [`extension/README.md`](extension/README.md) for Chromium and Firefox developer installation, supported URLs, tests, and troubleshooting.

## Architecture

```text
Axum HTTP server
    ↓
Playback session (in memory)
    ↓
MediaProvider trait
    ├── LocalFileProvider + FFmpeg
    ├── HttpFixtureProvider + streamed HTTP
    └── YtDlpProvider + yt-dlp/FFmpeg
              ↓
Shared FFprobe validation
              ↓
Temporary per-session storage
    ↓
Range-capable HTML5 playback
    ↓
Completion / timeout cleanup
```

Each session receives a UUID and can serve only the media path stored for that session. The cache is not mounted as a static directory. Startup removes stale cache directories, and a periodic task removes abandoned in-memory sessions.

## Development

```bash
cargo fmt --check
cargo check --locked
cargo test --locked
```

Set `RUST_LOG=localtube=debug` for more verbose diagnostics.

### Provider troubleshooting

- fixture `connection refused`: start `cargo run --bin fixture_server` and verify `LOCALTUBE_FIXTURE_BASE_URL`.
- fixture metadata unavailable: the deterministic fixture currently serves playable metadata for `demo` only.
- yt-dlp not found: install yt-dlp or set `LOCALTUBE_YTDLP_PATH` to the executable path.
- yt-dlp metadata failure: verify the configured source template and that `{video_id}` is present.
- yt-dlp reports no JavaScript runtime: install Node.js, verify `node --version`, and keep `LOCALTUBE_YTDLP_JS_RUNTIME=node` (or specify `node:/absolute/path`).
- yt-dlp reports `Requested format is not available`: LocalTube allows metadata extraction to continue, but acquisition still requires yt-dlp to discover an authorized downloadable format. Check `yt-dlp --ignore-config --cookies-from-browser firefox --list-formats <URL>` and update yt-dlp if formats are unexpectedly absent.
- yt-dlp acquisition failure: inspect LocalTube logs for yt-dlp stderr diagnostics.
- FFmpeg generation/merge failure: ensure `ffmpeg` is installed and available on `PATH`.
- validation failure: ensure `ffprobe` is installed and available on `PATH`.
- acquisition timeout: increase `LOCALTUBE_ACQUISITION_TIMEOUT` only for trusted slow sources.
