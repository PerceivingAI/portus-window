# Portus Window Windows Implementation

Portus Window runs natively on Windows 10 and 11 using Microsoft WebView2, Win32 window management, and Windows Named Pipes. It speaks the same protocol v7 and offers the same command interface as the Linux build; only the platform layers differ. The installed executables are `portus-window.exe` (the host) and `portus-window-cli.exe` (the controller). There is no separate daemon binary.

## Architecture

```text
agent or user -> portus-window-cli -> Named Pipe -> portus-window -> WebView2 windows
```

The host is a persistent Tauri 2 process containing:

- an async IPC coordinator (tokio named-pipe server);
- the window manager/registry (Win32 geometry and state);
- the Windows virtual-desktop workspace service;
- the media authority (`portus-media://` scheme handler);
- the web-video authority (loopback TCP engine);
- the persistent web profile store;
- the Firefox cookie broker;
- a dedicated SQLite worker for history and configuration.

## IPC transport

The host listens on the named pipe `\\.\pipe\portus-window`. Override it with `PORTUS_WINDOW_PIPE` or `PORTUS_WINDOW_SOCKET`, or `--socket`. A plain filename is normalized to `\\.\pipe\<filename>` automatically.

- The server creates its first pipe instance at startup with `first_pipe_instance(true)`. Each accepted client gets its own async worker task, and the next pipe instance is prepared immediately, so multiple CLI invocations can run concurrently.
- Requests and responses are newline-terminated JSON frames, bounded at 64 KB, using protocol v7 only. Older protocol versions are rejected; there is no compatibility parsing.
- The CLI applies a global IPC timeout (`--timeout-ms`, default 5000 ms).

Every active window has one identity, `wsess_<32 lowercase hex>`. The same value is the protocol identity, the native Tauri window label, and the active registry key. It is generated once per window and never recycled while the host runs. An exact `wsess_...` target never falls through to description, title, or URL matching; a stale ID is simply `target_not_found`.

## Host lifecycle

The host runs in the background even when no windows are open:

- When all content windows close, the process keeps running and listening on the pipe (exit is prevented through Tauri's `RunEvent::ExitRequested`).
- The host exits gracefully on Ctrl+C (console control signal) or via normal process management.

## Webview engine

Content windows use Microsoft WebView2, backed by the installed Edge Evergreen runtime (the loader is bundled by Tauri 2):

- Hardware-accelerated rendering through Direct3D/DirectComposition.
- HTML5 media with native Windows codec support (AAC, MP3, MP4 H.264, WebM VP8/VP9, WAV, FLAC).
- Platform-specific operations use the WebView2/Win32 layer directly.

Remote pages are restricted to HTTP and HTTPS. Popup and new-window requests are denied. Remote pages never receive a privileged bridge or filesystem authority.

## Local media

Generic `file://` access is not used. Local media files are served through the custom `portus-media://` URI scheme registered by the host:

1. The requested path is validated and the file is opened through a retained read-only handle.
2. Content is checked against a passive-media allowlist (PNG, JPEG, GIF, WebP, MP4, WebM, WAV, FLAC) by extension and file signature, with size limits applied.
3. An opaque random token bound to the owning `wsess_...` window authorizes delivery; the webview never receives a filesystem path or handle.
4. Authorization and the retained handle are revoked on failed construction, rollback, close, and destroy.

Directories, special files, and non-allowlisted content fail closed. Playback controls (play, pause, seek, volume) are typed protocol operations, not JavaScript calls.

## Window and workspace management

Geometry uses Win32 logical pixels: `width`/`height` in the 1-16384 range, and multi-monitor `x`/`y` desktop coordinates. Window state maps to native operations: maximize, minimize, restore, fullscreen, and always-on-top.

Virtual desktops are exposed by the Windows workspace service:

- **Desktop indexes are 1-based on Windows**: `1` is the first desktop, `2` the second, and so on. The underlying Windows virtual-desktop API is zero-based; the service translates between the native index and the CLI-facing 1-based index.
- The service tracks concrete desktop placement per active window and exposes the catalog through `portus-window-cli workspaces`.

Note the platform difference: Windows indexes are 1-based, Linux/X11 indexes are 0-based. Always inspect `workspaces` when the desktop count is unknown.

## Capture pipeline

Screenshots use native Win32 capture:

1. Resolve the window handle (`HWND`) and rectangle (`GetWindowRect`).
2. Create a compatible memory device context and bitmap.
3. Snapshot the full rendered surface with `PrintWindow` (`PW_RENDERFULLCONTENT`).
4. Extract 32-bit pixels with `GetDIBits` and encode PNG.

Output paths must be absolute `.png` paths. Overwrite is safe: without `--overwrite`, an existing destination fails closed; with `--overwrite`, the file is written to a temporary file in the destination folder and atomically moved into place. Video recording captures the window surface for a bounded duration to `.mp4`.

## DOM interaction

`interact` dispatches ordered, typed DOM actions atomically against an open web window:

- `wait_for_selector`: wait for a CSS selector to appear.
- `wait_for_text`: wait for text to appear in the document or an element.
- `click`: scroll into view, focus, and click an element.
- `fill`: clear, focus, and fill an input/textarea/contenteditable, dispatching `input`/`change`.
- `press_key`: dispatch `keydown`/`keypress`/`keyup`.
- `check_selector` / `check_text`: non-waiting assertions.

Each step returns a typed status: `ok`, `selector_not_found`, `selector_not_interactable`, `text_not_found`, `timeout`, or `script_error`. Batch deadlines are bounded (100-60000 ms). Agent-supplied values enter the page only as validated JSON data; raw JavaScript is never concatenated or executed.

## Authenticated browser sessions

Firefox is the supported browser source for authenticated-session import. Chromium, Chrome, and Brave fail closed until an encryption/keyring-aware broker exists.

- The agent can request, inspect, apply, and revoke grants; they can never approve or deny them.
- A trusted local consent dialog shows the exact browser, domain/scope, destination window, reason, and provenance.
- The Firefox broker resolves `%APPDATA%\Mozilla\Firefox\`, reads `profiles.ini` for the default profile, and opens `cookies.sqlite` strictly read-only (`PRAGMA query_only = ON`), querying only the authorized canonical domain.
- Applying a grant rebuilds the destination window on an isolated private profile under `%LOCALAPPDATA%\portus-window\auth-session-profiles\<window_session_id>\`.
- Cookies are never serialized, logged, persisted as general data, or returned to the agent. Revoke, close, and destroy reconcile imported cookies, broker material, authority state, and the isolated profile.
- Managed YouTube switches from the anonymous `youtube-nocookie.com` path to the explicit `youtube.com` path only for windows with an applied grant; revoking restores the anonymous path.

## Storage layout

All persistent state lives under `%LOCALAPPDATA%\portus-window\`:

```text
%LOCALAPPDATA%\portus-window\
  web-profile\                                  # persistent web profile (cookies, storage)
  profiles\<NAME>\                              # named isolated profiles (--profile)
  auth-session-profiles\<window_session_id>\    # isolated authenticated-session profiles
  history.sqlite3                               # closed-window history and configuration
```

Never persisted: media authorization tokens, internal media URLs, retained handles, media bytes, playback state, console buffers, cookie values, authenticated-session secret material, or ephemeral YouTube loopback tokens.

## Persistence

SQLite schema version 3 is owned by a dedicated database worker thread. UI callbacks enqueue state; they never execute SQLite inline. Closed history rows carry a persisted record identity distinct from the active `wsess_...` identity. Disabling history preserves already-closed history, drops active tracking, and does not backfill when re-enabled. `history --clear` removes closed history only.

## Examples

```powershell
# Check the host
portus-window-cli ping

# Open a page and wait for load
portus-window-cli open "https://example.com" --wait-loaded

# Inspect active windows
portus-window-cli list

# Interact and prove the result
portus-window-cli interact wsess_00000000000000000000000000000001 `
  --action '{"kind":"wait_for_selector","selector":"h1"}' `
  --action '{"kind":"check_text","text":"Example Domain"}' `
  --screenshot-out result.png --screenshot-overwrite

# Arrange the desktop
portus-window-cli resize wsess_00000000000000000000000000000001 --width 1280 --height 720 --x 100 --y 100
portus-window-cli resize --all --minimize
portus-window-cli close --all
```

## Intentional limits

The Windows implementation does not provide generic filesystem browsing, arbitrary JavaScript execution, popup escape, Chromium-family session import, a separate daemon executable, hidden prewarm windows, or compatibility with older protocol versions.

