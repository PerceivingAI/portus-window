---
name: portus-window
description: Control Portus Window (portus-window-cli) to visually present web pages and local media, run lightweight DOM interactions, capture screenshots and video recordings, manage bulk window state (--all), isolated persistent profiles, declarative workspace specs (apply / export), and inspect authentication status on Linux/X11 and Windows.
version: 0.2.0
author: PerceivingAI
license: Apache-2.0
compatibility:
  os: linux, windows
  display: x11, windows
tools:
  - portus-window-cli
keywords:
  - webview
  - browser
  - media-player
  - screenshot
  - screen-record
  - declarative-windows
  - bulk-windows
  - dom-interaction
  - x11
  - windows
---

# Portus Window Controller (`portus-window-cli`)

`portus-window-cli` is the authoritative control interface for **Portus Window**, a lightweight presentation, inspection, and window automation system for AI agents.

---

## 1. Platform & IPC Transport

Portus Window runs natively on both **Windows** and **Linux**:

- **Linux (X11 / i3)**: Communicates over Unix domain socket `/tmp/portus-window.socket` (override via `PORTUS_WINDOW_SOCKET`).
- **Windows (10 / 11)**: Communicates over Windows Named Pipe `\\.\pipe\portus-window` (override via `PORTUS_WINDOW_PIPE` or `PORTUS_WINDOW_SOCKET`).
- **Protocol**: Protocol v7 newline-delimited JSON frames up to $64\,\text{KB}$ (`MAX_FRAME_BYTES`).

---

## 2. Command Reference

### A0. Batch Window Opening (`open-batch`)
Open multiple independent windows with one CLI invocation. The manifest is a JSON array using the same source, profile, description, and geometry fields as individual window specifications. This command only creates windows; it does not reconcile or prune existing windows.

```bash
portus-window-cli open-batch windows.json
cat windows.json | portus-window-cli open-batch -
```

Example:
```json
[
  { "source": { "kind": "web", "url": "https://www.youtube.com" } },
  { "source": { "kind": "web", "url": "https://example.com" }, "geometry": { "width": 500, "height": 400, "x": 800, "y": 0 } }
]
```

The command supports up to 32 windows per request and returns the created `window_session_id` values. Use `apply` when declarative reconciliation/update/prune behavior is required.### A. Declarative Workspace Management (`apply` / `export`)
Apply or export entire multi-window environments idempotently in a single turn. `apply -f FILE` reads a file; `apply -` or omitted `-f` reads STDIN. Add `--prune` to close active windows absent from the spec.

```bash
# Apply layout from file:
portus-window-cli apply -f layout.json [--prune]

# Apply layout from STDIN pipe:
cat << 'EOF' | portus-window-cli apply -
{
  "version": 1,
  "windows": [
    {
      "name": "dashboard",
      "source": { "kind": "web", "url": "https://example.com" },
      "profile": "prod-monitoring",
      "geometry": { "width": 1280, "height": 720, "x": 0, "y": 0, "workspace": { "kind": "index", "index": 1 } }
    },
    {
      "name": "media-player",
      "source": { "kind": "local_media", "path": "/path/to/video.mp4" },
      "geometry": { "width": 640, "height": 480, "x": 1300, "y": 0, "always_on_top": true },
      "media": { "kind": "play" }
    }
  ]
}
EOF

# Export current desktop layout to file:
portus-window-cli export -o backup_layout.json
```

---

### B. Lifecycle & Content Operations
```bash
# Check daemon availability and protocol version:
portus-window-cli ping

# Open an HTTP/HTTPS URL or an explicitly admitted local media path.
# Initial window geometry, state, always-on-top, and workspace may be supplied atomically at creation. Prefer this over opening first and resizing/maximizing afterward when the desired initial state is known:
portus-window-cli open "<SOURCE>" [--description "<TAG>"] [--profile "<NAME>"] [--wait-loaded] \
  [--width <WIDTH>] [--height <HEIGHT>] [--x <X>] [--y <Y>] \
  [--maximize | --minimize | --restore | --fullscreen] \
  [--always-on-top <true|false>] [--workspace <INDEX_OR_NAME>]

# The state flags are mutually exclusive. Geometry/position options can be combined with a state flag.
# --fullscreen is native Portus window fullscreen, not HTML/WebKit media fullscreen.
portus-window-cli open "/path/to/video.mp4" --width 1280 --height 720 --maximize
portus-window-cli open "/path/to/video.mp4" --fullscreen --wait-loaded

# Close a single window or all active windows:
portus-window-cli close <TARGET>
portus-window-cli close --all
```

---

### C. Observability & Inspection
```bash
# List all active windows with geometry, titles, URLs, and states:
portus-window-cli list

# Retrieve live observed status for a single window:
portus-window-cli status <TARGET>

# Fetch bounded console error logs from web windows:
portus-window-cli console <TARGET>
portus-window-cli console --all

# Capture hardware-accelerated PNG screenshot:
portus-window-cli screenshot <TARGET> --out /path/to/capture.png [--overwrite]
portus-window-cli screenshot --all --out /path/to/batch_prefix_

# Record window surface video clip (duration 0.1..600.0s):
portus-window-cli record <TARGET> --out /path/to/video.mp4 [--duration-seconds 30.0] [--overwrite]
```

---

### D. DOM Automation (`interact`) — Web Only
Execute atomic batches of ordered typed DOM interactions. This command is available only for ordinary web windows; it does not control local-media windows or YouTube/web-video playback. Use `fill` with a `value` for text input; `type_text` is not a supported action kind:

```bash
portus-window-cli interact <TARGET> \
  --action '{"kind":"wait_for_selector","selector":"#search-input"}' \
  --action '{"kind":"fill","selector":"#search-input","value":"query text"}' \
  --action '{"kind":"press_key","key":"Enter","selector":"#search-input"}' \
  --action '{"kind":"wait_for_text","text":"Search Results"}' \
  --action '{"kind":"click","selector":".result-item"}' \
  --action '{"kind":"check_text","text":"Product Details"}' \
  --interaction-timeout-ms 5000 \
  --screenshot-out /path/to/result.png \
  --screenshot-overwrite
```

#### Supported Action Kinds:
- `wait_for_selector`: `{"kind":"wait_for_selector","selector":"<CSS_SELECTOR>"}`
- `wait_for_text`: `{"kind":"wait_for_text","text":"<EXPECTED_TEXT>","selector":"<OPTIONAL_ROOT>"}`
- `click`: `{"kind":"click","selector":"<CSS_SELECTOR>"}`
- `fill`: `{"kind":"fill","selector":"<CSS_SELECTOR>","value":"<VALUE>"}`
- `press_key`: `{"kind":"press_key","key":"<KEY>","selector":"<OPTIONAL_TARGET>"}`
- `check_selector`: `{"kind":"check_selector","selector":"<CSS_SELECTOR>"}`
- `check_text`: `{"kind":"check_text","text":"<EXPECTED_TEXT>","selector":"<OPTIONAL_ROOT>"}`

---

### E. Window Geometry & State Control
```bash
# Resize, reposition, or change window state:
portus-window-cli resize <TARGET> \
  [--width <WIDTH>] [--height <HEIGHT>] \
  [--x <X>] [--y <Y>] \
  [--maximize | --minimize | --restore | --fullscreen] \
  [--always-on-top <true|false>] \
  [--workspace <INDEX_OR_NAME>]

# State flags are mutually exclusive. At least one geometry/state operation is required.
# Geometry, position, workspace, always-on-top, and state can be combined in one command.
# --restore returns a maximized, minimized, or fullscreen window to normal state.
portus-window-cli resize --all --minimize
portus-window-cli resize --all --restore
portus-window-cli resize --all --fullscreen
portus-window-cli resize --all --workspace 1

# Focus and raise window:
portus-window-cli focus <TARGET>

# Tag window with custom context description:
portus-window-cli tag <TARGET> "<DESCRIPTION>"
portus-window-cli tag --all "<DESCRIPTION>"
```

---

### F. Browser Credential Permission (`auth-session`)
Request and manage brokered browser credentials. For the on-demand browser-credential path, target the **exact open Portus Window** with `--window`. The `request` command displays the user consent modal on that target window; the agent cannot approve the request.

```bash
portus-window-cli auth-session request --window <WINDOW_SESSION_ID> --browser <firefox|chromium|chrome|brave> [--scope <once|session|remembered>] [--reason "<REASON>"]
portus-window-cli auth-session status --window <WINDOW_SESSION_ID>
portus-window-cli auth-session status --domain <DOMAIN>
portus-window-cli auth-session apply --window <GRANT_WINDOW_ID_OR_DOMAIN> --window-session-id <DESTINATION_WINDOW_SESSION_ID>
portus-window-cli auth-session revoke --window <WINDOW_SESSION_ID>
portus-window-cli auth-session revoke --domain <DOMAIN>
```

Rules:
- Use `auth-session request --window ...` when the user explicitly asks the agent to use credentials from their browser on an already-open Portus Window.
- The consent UI is user-controlled. There is no agent command to approve it.
- Denying, dismissing, clicking outside the modal, or closing the target window does not grant permission.
- Keep the target window ID exact; do not substitute another window.
- `--scope session` is the default.

### G. Media Playback Controls
Control local audio/video playback windows only. YouTube and other web video windows remain web content; use `interact` only for their bounded DOM surface, and do not send them `media` commands:
```bash
# Play / Pause:
portus-window-cli media <TARGET> play
portus-window-cli media <TARGET> pause
portus-window-cli media --all pause

# Seek to seconds:
portus-window-cli media <TARGET> seek --seconds 45.0

# Set volume level (0.0 to 1.0):
portus-window-cli media <TARGET> set-volume --level 0.75

# Local video windows use Portus-owned controls (play/pause, timeline, volume/mute, fullscreen).
# Do not restore native HTML media controls; native WebKit media fullscreen is not used.
```

Local video behavior:
- Clicking the video surface toggles play/pause.
- The custom control bar provides play/pause, seeking, mute/volume, and fullscreen.
- Controls auto-hide after 2 seconds of inactivity while playing and reappear on pointer movement or interaction.
- The fullscreen control uses Portus window fullscreen; it does not invoke browser-native media fullscreen.

---

### H. Workspace Indexing on Windows

Windows virtual desktops are exposed to agents and the CLI using **1-based indexes**: `1` is the first desktop, `2` is the second, `3` is the third, and so on. The native Windows API is internally zero-based, but agents must never use the native zero-based values when calling Portus Window.

Linux/X11 remains **0-based**: `0` is the first desktop, `1` is the second, `2` is the third.

Always use `portus-window-cli workspaces` to inspect the available catalog before targeting an index when the desktop count is unknown. For example, on Windows with three desktops, `--workspace 1` targets Desktop 1, `--workspace 2` targets Desktop 2, and `--workspace 3` targets Desktop 3.

### H. Agent Usage Rules

- `auth-session request --window <WINDOW_SESSION_ID>` is the on-demand path for asking the user to allow browser credentials on an already-open window. The consent modal is rendered on that target window and only the user can approve it.

- Use `open` with the desired initial size, position, state, always-on-top setting, and workspace whenever possible; these are applied as one initial window specification.
- Use `resize` for later window changes. Use `--restore` to leave maximized, minimized, or fullscreen state.
- `--maximize`, `--minimize`, `--restore`, and `--fullscreen` are mutually exclusive within a single `open` or `resize` command.
- `resize` requires either a target or `--all`, plus at least one geometry/state operation.
- `interact` is for ordinary web windows only. Do not use it for local-media playback.
- Local media must be opened as a filesystem path; `file://` URLs are rejected.
- For local video, the Portus-owned controls are authoritative: the video surface toggles play/pause, the custom bar provides play/pause, seek, mute/volume, and fullscreen, and controls auto-hide after 2 seconds while playing. Do not rely on native browser media controls.

### I. Configuration & Persistent History
```bash
# List virtual desktops / workspaces:
portus-window-cli workspaces

# Query closed window history:
portus-window-cli history [--query "<SEARCH_QUERY>"]

# Purge closed history logs:
portus-window-cli history --clear

# Show or update daemon configuration:
portus-window-cli config --show
portus-window-cli config --set history_enabled=<true|false>
portus-window-cli config --set retention_days=<1..3650|null>
```

---

## 3. Targeting & Identity Model

- **Canonical Active Window ID**: `wsess_<32-lowercase-hex>` (e.g. `wsess_df872af90784469b9ad060ffc6d5f0ad`).
- **Target Resolution Precedence**:
  1. Exact canonical ID (`wsess_...`) $\rightarrow$ exact lookup only (never falls through to fuzzy matching).
  2. Description tag substring.
  3. Window title substring.
  4. Active URL substring.

---

## 4. Exit Codes Reference

| Exit Code | Classification | Meaning |
| :---: | :--- | :--- |
| **`0`** | `Success` | Command executed successfully. |
| **`1`** | `ValidationFailed` | Invalid arguments, malformed JSON, out-of-range timeout/duration, or unsupported scheme. |
| **`2`** | `VersionMismatch` / `ClapConflict` | Protocol version incompatibility or conflicting CLI arguments (e.g. `<TARGET>` + `--all`). |
| **`3`** | `DaemonUnavailable` | Daemon host is not running or failed to respond on the socket/pipe. |
| **`4`** | `TargetNotFound` / `TargetAmbiguous` | Target window session ID not found or query matched multiple active windows. |
| **`5`** | `Timeout` | Operation or DOM interaction batch deadline elapsed before completion. |
| **`6`** | `OperationFailed` | Media playback error, native screenshot failure, or DOM interaction assertion failure. |
| **`7`** | `Internal` | Internal host daemon error or invalid database schema. |

---

## 5. Security & Isolation Boundaries

- **Web Security**: Top-level remote content is strictly `http://` or `https://`. Generic `file://` URLs and unsupported schemes (`ftp://`) are denied.
- **Local Media Admission**: Local files are passed as explicit filesystem paths. Media is validated as passive content (PNG, JPEG, GIF, WebP, MP4, WebM, WAV, FLAC), served over isolated `portus-media://` tokens, and revoked on window cleanup.
- **Profile Isolation**: `--profile <NAME>` creates an isolated browser storage tier under `%LOCALAPPDATA%\portus-window\profiles\<NAME>\` (Windows) or `~/.local/share/portus-window/profiles/<NAME>/` (Linux).
- **Zero Secret Leakage**: Raw browser cookies, auth tokens, and session headers are never printed in CLI outputs, stored in SQLite, or logged.
