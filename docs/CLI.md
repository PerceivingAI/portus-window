# Portus Window CLI (`portus-window-cli`)

`portus-window-cli` is the control interface for Portus Window. Every command is a single process invocation that sends one bounded JSON request to the running `portus-window` host and prints the result which makes it directly usable from agent tool calls as well as from a normal shell.

## Global options

```text
portus-window-cli [OPTIONS] <COMMAND> [COMMAND_OPTIONS]

--socket <PATH>      Unix domain socket (Linux) or named pipe (Windows) of the host.
                     Default: /tmp/portus-window.socket (Linux) or \\.\pipe\portus-window (Windows).
                     Also settable through PORTUS_WINDOW_SOCKET (and PORTUS_WINDOW_PIPE on Windows).
--timeout-ms <MS>    Global IPC timeout in milliseconds (default 5000, minimum 1).
```

## Commands

| Command | Purpose |
| --- | --- |
| `ping` | Check host availability and protocol version. |
| `open <SOURCE>` | Open a web URL or local media file in a new window. |
| `open-batch <FILE>` | Open up to 32 windows from a JSON manifest (`-` or omitted reads STDIN). |
| `apply [-f FILE] [--prune]` | Reconcile the desktop to a declarative workspace specification. |
| `export [-o FILE]` | Export the current window layout as a declarative specification. |
| `list` | List active windows with live titles, URLs, and states. |
| `status <TARGET>` | Observed status, title, URL, auth state, and console errors for one window. |
| `console <TARGET> [--all]` | Fetch bounded console output for web windows. |
| `screenshot <TARGET> [--all] --out <FILE.png>` | Capture a window surface to PNG. |
| `record <TARGET> --out <FILE.mp4> [--duration-seconds <N>]` | Record the window surface to video. |
| `interact <TARGET> --action '<JSON>' ...` | Run an ordered batch of typed DOM actions. |
| `media <TARGET> [--all] <play\|pause\|seek\|set-volume>` | Control local audio/video playback. |
| `tag <TARGET> [--all] "<DESCRIPTION>"` | Set a window's context description. |
| `focus <TARGET>` | Focus and raise a window. |
| `resize <TARGET> [--all] [OPTIONS]` | Change size, position, workspace, or state. |
| `close <TARGET> [--all]` | Close one or all windows. |
| `history [--query <STR>] [--clear]` | Search or purge closed-window history. |
| `config [--show] [--set <KEY>=<VALUE>]` | Inspect or update runtime configuration. |
| `workspaces` | List virtual desktops. |
| `auth-session <request\|status\|apply\|revoke>` | Brokered browser-credential grants (Firefox). |

## Opening windows

```bash
# A web page (HTTP/HTTPS only)
portus-window-cli open "https://example.com" --wait-loaded

# Local media (PNG, JPEG, GIF, WebP, MP4, WebM, WAV, FLAC)
portus-window-cli open /home/user/demo.mp4

# With initial geometry, state, and placement
portus-window-cli open "https://example.com" \
  --width 1280 --height 720 --x 0 --y 0 \
  --maximize --always-on-top true \
  --description "docs" --profile work --wait-loaded

# Many windows at once
portus-window-cli open-batch windows.json
```

`open` returns the new window's `window_session_id`. Media commands apply to local media windows only, not to YouTube or other web video.

## Declarative layouts (`apply` / `export`)

Apply an entire multi-window environment idempotently in one call. `--prune` closes active windows that are absent from the specification.

```bash
portus-window-cli apply -f layout.json [--prune]

cat << 'EOF' | portus-window-cli apply -
{
  "version": 1,
  "windows": [
    {
      "name": "dashboard",
      "source": { "kind": "web", "url": "https://example.com" },
      "profile": "prod",
      "geometry": { "width": 1280, "height": 720, "x": 0, "y": 0, "workspace": { "kind": "index", "index": 1 } }
    },
    {
      "name": "clip",
      "source": { "kind": "local_media", "path": "/home/user/clip.mp4" },
      "geometry": { "width": 640, "height": 480, "x": 1300, "y": 0, "always_on_top": true },
      "media": { "kind": "play" }
    }
  ]
}
EOF

# Save the current desktop for later restoration
portus-window-cli export -o backup.json
```

## Targeting windows

`<TARGET>` accepts, in precedence order:

1. **Exact canonical ID**: `wsess_<32-hex>`. Exact lookup only; never falls through to fuzzy matching. A stale ID is `target_not_found`.
2. **Description tag** substring the value set by `open --description` or `tag`.
3. **Window title** substring.
4. **Active URL** substring.

If a non-exact selector matches multiple windows, the operation fails as ambiguous. Append `--all` instead of `<TARGET>` to apply an operation to every eligible window.

## Bulk operations (`--all`)

```bash
portus-window-cli resize --all --minimize
portus-window-cli resize --all --restore
portus-window-cli resize --all --workspace 2
portus-window-cli media --all pause
portus-window-cli tag --all "project-review"
portus-window-cli screenshot --all --out /tmp/shots/
portus-window-cli close --all
```

## DOM interaction (web windows only)

```bash
portus-window-cli interact wsess_0123abcd... \
  --action '{"kind":"wait_for_selector","selector":"#submit-btn"}' \
  --action '{"kind":"fill","selector":"#input","value":"Ada"}' \
  --action '{"kind":"click","selector":"#submit-btn"}' \
  --action '{"kind":"wait_for_text","text":"Saved"}' \
  --interaction-timeout-ms 10000 \
  --screenshot-out /tmp/after_submit.png
```

Available action kinds: `wait_for_selector`, `wait_for_text`, `click`, `fill`, `press_key`, `check_selector`, `check_text`. Batch timeout bounds: 100 60000 ms. There is no arbitrary JavaScript execution.

## Media control (local media windows only)

```bash
portus-window-cli media wsess_0123abcd... play
portus-window-cli media wsess_0123abcd... pause
portus-window-cli media wsess_0123abcd... seek --seconds 45.0
portus-window-cli media wsess_0123abcd... set-volume --level 0.75
```

## Workspaces

```bash
portus-window-cli workspaces                       # inspect the catalog first
portus-window-cli open "https://example.com" --workspace 1
portus-window-cli resize wsess_0123abcd... --workspace "code"
```

Workspace indexing is platform-specific:

- **Windows:** 1-based (`1` = first desktop).
- **Linux/X11:** 0-based (`0` = first desktop).

Name selectors resolve by exact case-insensitive match; ambiguous names fail closed.

## History and configuration

```bash
portus-window-cli history [--query "example"]
portus-window-cli history --clear                  # removes closed history only
portus-window-cli config --show
portus-window-cli config --set history_enabled=<true|false>
portus-window-cli config --set retention_days=<1..3650|null>
```

## Authenticated browser sessions (Firefox)

```bash
# Ask the user for consent (the user, not the agent, approves)
portus-window-cli auth-session request --browser firefox --domain example.com --scope session --reason "Sign in to the dashboard"

# Inspect a grant
portus-window-cli auth-session status --domain example.com

# Apply an approved grant to an open window
portus-window-cli auth-session apply --domain example.com --window-session-id wsess_0123abcd...

# Revoke
portus-window-cli auth-session revoke --domain example.com
```

Chromium-family browsers fail closed. Raw cookies and tokens are never printed or returned.

## Exit codes

| Code | Meaning |
| :--- | :--- |
| `0` | Success |
| `1` | Validation error (bad arguments, malformed JSON, out-of-range timeout, unsupported scheme) |
| `2` | Protocol version mismatch or conflicting arguments |
| `3` | Host unavailable / invalid IPC response |
| `4` | Target not found or ambiguous target match |
| `5` | Operation timeout |
| `6` | Runtime operation failed (media, screenshot, resize, interaction assertion) |
| `7` | Internal host error / invalid database schema |

