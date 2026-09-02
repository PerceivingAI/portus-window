# Portus Window Linux Implementation

Portus Window runs natively on Linux against an X11 desktop. The primary target is an Artix/Arch-family system with the i3 window manager. Wayland is not supported. The installed executables are `portus-window` (the host) and `portus-window-cli` (the controller). There is no separate daemon binary.

## Architecture

```text
agent or user -> portus-window-cli -> Unix domain socket -> portus-window -> WebKitGTK windows
```

The host is a persistent Tauri 2 process. It builds one native WebKitGTK-backed content window per active session and stays alive when the last window closes, continuing to listen for IPC connections. Windows are constructed from focused modules that separate open lifecycle, close/destroy lifecycle, ordinary window commands, and authenticated-session transitions.

Runtime dependencies come from the host system: GTK3/GDK, WebKitGTK 2.x, GLib/GIO/libsoup, Cairo, X11/EWMH, and GStreamer (supplied through WebKitGTK) for media playback.

Autostart with i3:

```text
exec --no-startup-id portus-window
```

Portus Window never edits the user's i3 configuration.

## IPC transport

The host listens on a Unix domain socket, normally `/tmp/portus-window.socket`. Override it with `PORTUS_WINDOW_SOCKET` or `--socket`.

- Requests and responses are newline-terminated JSON frames, bounded at 64 KB, using protocol v7 only. Older protocol versions are rejected; there is no compatibility parsing.
- The socket is created with `0600` permissions and is owned by the launching user.
- A client that does not finish sending its request frame within five seconds receives a typed timeout error and cannot hold the IPC worker.

Every active window has one identity, `wsess_<32 lowercase hex>`. The same value is the protocol identity, the native Tauri window label, and the active registry key. It is generated once per window and never recycled while the host runs. An exact `wsess_...` target never falls through to description, title, or URL matching; a stale ID is simply `target_not_found`.

## Window construction and lifecycle

Window construction is split by responsibility: source-specific open logic with rollback, close/destroy reconciliation, ordinary window commands, authenticated-session replacement, and shared manager state.

- Safety hooks and the active-window registry are established before first visibility.
- Persistence and workspace bookkeeping may complete after the window becomes visible, but the IPC `open` result is fail-closed: if required bookkeeping fails, the window is destroyed or rolled back.
- Ordinary web windows start at `about:blank`, install the platform load-failure observer, and then navigate to the requested URL. Synthetic `about:blank` observations are ignored for page state.
- Load failure is observed through the WebKitGTK `load_failed` signal, registered before navigation. The state reducer preserves `failed` even if later `Started`/`Finished` events arrive for the same URL. Failures surface through normal status observability no synthetic timeouts or URL-specific workarounds.

Remote pages are restricted to HTTP and HTTPS. Popup and new-window requests are denied. Remote pages never receive a privileged Tauri bridge or filesystem authority.

## Persistent web profiles

Ordinary web windows share a persistent WebKitGTK profile managed by the host, stored under the Portus Window application data directory. `PORTUS_WINDOW_DATA_DIR` can override the base data directory (useful for isolated setups). Cache clearing removes temporary WebKit cache only; persistent cookies, local storage, and IndexedDB are preserved. No hidden prewarm windows are ever created.

## Local media

Generic `file://` access is not used. Local media is admitted only through the explicit local-media open path:

1. Rejects empty and oversized path input, and rejects symlinks before canonicalization.
2. Canonicalizes the path and verifies it is a regular file.
3. Opens a retained read-only handle with no-follow/nonblocking flags.
4. Validates extension and file signature against a passive-media allowlist (PNG, JPEG, GIF, WebP, MP4, WebM, WAV, FLAC).
5. Applies image-size and response-size limits.
6. Issues an opaque random token bound to the owning `wsess_...` window.

Directories, FIFOs, sockets, devices, and malformed or non-allowlisted content fail closed. Authorization and the retained handle are revoked on failed construction, rollback, close, and destroy.

### Media transport

WebKitGTK on this target does not reliably deliver media requests to a custom scheme handler, so Linux uses a daemon-owned loopback HTTP endpoint:

```text
http://127.0.0.1:<ephemeral-port>/<opaque-token>/view
```

- Bound to loopback only, started lazily.
- The token is the authorization capability; unknown tokens return `404`. The webview never receives a filesystem path or file handle.
- The `view` route returns a restrictive HTML document (strict CSP, no scripts) containing an image, audio, or video element; the relative `content` route streams from the retained handle.
- Only `GET`/`HEAD` are accepted. Delivery supports bounded byte ranges with `Accept-Ranges`/`Content-Range` and `200`/`206` responses, capped by a maximum response size.
- Playback controls (play, pause, seek, volume) are typed protocol operations, not JavaScript calls.

## X11 and workspace integration

The Linux `WorkspaceService` discovers EWMH desktops, names, active-desktop state, and window placement through X11 properties and events.

- Workspace indices are zero-based EWMH desktop indices (`0` is the first desktop).
- Name selectors match exactly, case-insensitively; ambiguous duplicate names fail closed.
- Movement uses `_NET_WM_DESKTOP` client messages, and completion is event-confirmed never direct property mutation or per-window polling.

## Capture and observability

Screenshots are captured natively from the WebKitGTK widget into a Cairo ARGB32 surface and encoded as PNG. Invalid allocations and capture failures are reported without clobbering the caller's output path.

Web observability includes the current URL, bounded URL history, document title, started/finished/failed load state, and bounded console-error capture.

## Managed YouTube

Managed YouTube is a web source, not local media. Presentation is wrapped by the daemon-owned loopback web-video engine with opaque per-window state and bounded typed controls. Anonymous presentation uses the privacy-enhanced `youtube-nocookie.com` embed path. After an authenticated-session grant is applied, the window switches to the explicit `youtube.com` path; revoking restores the anonymous path.

## Authenticated browser sessions

Firefox is the supported browser source for authenticated-session import. Chromium, Chrome, and Brave fail closed until an encryption/keyring-aware broker exists.

- The agent can request, inspect, apply, and revoke grants; they can never approve or deny them.
- A trusted local consent dialog shows the exact browser, domain/scope, destination window, reason, and provenance.
- Applying a grant rebuilds the destination window on an isolated private WebKit profile under `auth-session-profiles/<window_session_id>`.
- Cookies are scoped to the authorized domain and are never serialized, logged, persisted as general data, or returned to the agent.
- Revoke, close, and destroy reconcile imported cookies, broker material, authority state, and the isolated profile.

## Persistence

SQLite schema version 3 is owned by a dedicated database worker thread. WebKit/X11 callbacks enqueue state; they never execute SQLite inline.

- Closed history rows carry a persisted record identity distinct from the active `wsess_...` identity.
- Durable history may include the requested source; for local media this can reveal the requested filesystem path.
- Never persisted: media authorization tokens, internal media URLs, retained handles, media bytes, playback state, console buffers, cookie values, authenticated-session secret material, or ephemeral YouTube loopback tokens.
- Disabling history preserves already-closed history, drops active tracking, and does not backfill when re-enabled. `history --clear` removes closed history only.
- After an abrupt host restart, durable configuration is preserved and socket readiness is rejected until a live IPC connection succeeds.

## Intentional limits

The Linux implementation does not provide Wayland support, generic filesystem browsing, arbitrary JavaScript execution, popup escape, Chromium-family session import, a separate daemon executable, hidden prewarm windows, or compatibility with older protocol versions.

