# LocalTube Browser Extension

This Phase 2 WebExtension detects supported YouTube watch-page navigation, checks the local LocalTube backend, and redirects the tab to its existing player endpoint. It does not download, process, store, or play media itself.

```text
YouTube /watch URL → extract video ID → check /health → LocalTube /watch
```

## Requirements

- The LocalTube backend from this repository
- Chromium, Chrome, Edge, or Firefox with extension developer tools
- Media that the Phase 1 backend is authorized and configured to provide

Start the backend from the repository root:

```bash
cargo run
```

Verify it is available:

```bash
curl http://127.0.0.1:6969/health
```

The expected response is:

```json
{"status":"ok"}
```

The backend address is centralized in `config.js` as `LOCAL_BASE_URL` and defaults to `http://127.0.0.1:6969`.

## Install in Chromium-based browsers

1. Open `chrome://extensions` (or the equivalent extension manager).
2. Enable **Developer mode**.
3. Select **Load unpacked**.
4. Choose this `extension/` directory.
5. Start LocalTube and open a supported YouTube URL.

## Install temporarily in Firefox

1. Open `about:debugging#/runtime/this-firefox`.
2. Select **Load Temporary Add-on**.
3. Choose `extension/manifest.json`.
4. Start LocalTube and open a supported YouTube URL.

The manifest uses Manifest V3. It supplies both Chromium's `background.service_worker` and Firefox's `background.scripts` declarations to isolate the principal browser difference.

## Supported URLs

Supported:

```text
https://www.youtube.com/watch?v=abc123
https://www.youtube.com/watch?v=abc123&list=xyz
https://youtube.com/watch?v=abc123
https://m.youtube.com/watch?v=abc123
```

Ignored:

- YouTube home, search, channel, and playlist-only pages
- YouTube Shorts
- Non-YouTube hosts
- HTTP YouTube URLs
- Missing or malformed video IDs
- LocalTube URLs

Video IDs must contain only ASCII letters, digits, `_`, or `-`, up to 128 characters, matching the backend's Phase 1 validation.

## How navigation works

- Full document navigation is detected by `webNavigation.onCommitted`.
- YouTube SPA navigation is reported by the content script using YouTube's `yt-navigate-finish` event.
- `popstate` handles browser history traversal.
- Duplicate events for the same tab/video are suppressed briefly.
- The extension calls `GET http://127.0.0.1:6969/health` before redirecting.
- If health returns `{"status":"ok"}`, the tab navigates to `http://127.0.0.1:6969/watch?v=<VIDEO_ID>`.
- If the backend is unavailable, the YouTube page remains open and a clear message is written to the extension's background console.

No polling, page-provided code execution, or arbitrary HTML injection is used.

## Tests

URL parsing uses plain JavaScript and Node's built-in test runner, with no package installation required:

```bash
cd extension
npm test
```

The test suite covers supported watch URLs, extra query parameters, non-video pages, Shorts, malformed URLs, missing IDs, untrusted hosts, and LocalTube loop prevention.

## Troubleshooting

### The page does not redirect

1. Confirm `curl http://127.0.0.1:6969/health` returns `{"status":"ok"}`.
2. Confirm the extension is enabled and has access to YouTube and `127.0.0.1`.
3. Reload the unpacked extension after editing its files.
4. Inspect the extension service worker/background console for `[LocalTube]` messages.
5. Ensure the URL is a supported `/watch?v=...` URL, not a Short or playlist-only page.

### The LocalTube page says media is unavailable

The redirect succeeded, but the backend could not find authorized media for that video ID. This is a backend/provider concern; the extension intentionally does not acquire media.

### A custom backend host or port is used

Update `LOCAL_BASE_URL` in `config.js` and the matching backend entry in `manifest.json` `host_permissions`, then reload the extension.
