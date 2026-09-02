# Portus Window

Portus Window gives AI agents a lightweight, fast surface to display content directly to the user. The agent can open multiple web pages or local media as desktop windows, then inspect, interact, capture, and arrange them through a CLI.

The agent owns the surface: windows open on demand, are driven entirely by commands, and close when done.

## How it works

```text
agent -> portus-window-cli -> local IPC -> portus-window host -> visible windows
```

- **`portus-window`**: the host. Owns every visible window, enforces all security boundaries, keeps running between commands.
- **`portus-window-cli`**: the controller. One invocation sends one bounded JSON request over local IPC.
- **`portus-window-protocol`**: shared type-safe protocol definitions (protocol v7).

Transport is newline-terminated JSON frames over a local Unix domain socket (Linux) or named pipe (Windows). Every active window has one stable identity, `wsess_<32-hex>`, shared by the protocol, the window system, and the internal registry.

## What agents can do

- **Present content**: open HTTP/HTTPS pages or local images/audio/video, with size, position, workspace, and window state.
- **Observe**: read live title, URL, load state, and bounded console errors. Capture PNG screenshots or video recordings.
- **Interact**: run typed DOM actions (wait, click, fill, press keys, assert) against open web windows. No arbitrary JavaScript.
- **Control media**: play, pause, seek, and set volume on local audio/video windows.
- **Arrange the desktop**: focus, resize, move between virtual desktops, minimize/maximize, pin on top. One window or all at once. Reconcile whole layouts declaratively (`apply` / `export`).
- **Persist and recall**: query closed-window history and runtime configuration.

## Security

- Remote content is HTTP/HTTPS only. `file://` is never used.
- Local media is validated as passive content, admitted through a retained read-only handle, and served over per-window authorization that is revoked when the window closes.
- Remote pages get no privileged bridge and no filesystem authority. Popups are denied.
- Authenticated browser-session import (Firefox) needs explicit on-screen user consent. The agent can request it, never approve it. Cookies and tokens are never exposed.
- All IPC is local, framed, and size-bounded.

## Platforms

- **Linux**: X11 (i3 is the reference window manager), WebKitGTK. Primary target. See `docs/LINUX.md`.
- **Windows**: 10/11, WebView2/Win32. See `docs/WINDOWS.md`.

Wayland is not currently supported.

## Quick start

````bash
# Start the host (or let your session autostart it)
portus-window &

# Check connectivity
portus-window-cli ping

# Show the user a page
portus-window-cli open "https://example.com" --wait-loaded

# Show a local video
portus-window-cli open /home/user/demo.mp4

# Prove what the user is seeing
portus-window-cli screenshot wsess_0123abcd... --out /tmp/proof.png

# Clean up
portus-window-cli close --all
```

## Install the agent skill

The repo ships an agent skill so agent runtimes can discover Portus Window and its commands automatically.

Install it with the skills CLI:

```bash
npx skills add PerceivingAI/portus-window
```

Or install it manually from a checkout:

```bash
./scripts/install_skill.sh          # global (~/.claude, ~/.agents, and ~/.codex skills)
./scripts/install_skill.sh --local  # current project only
```

The platform installers install the skill too: scripts/install.ps1 on Windows and scripts/get-portus.sh on Linux place it under ~/.claude/skills/portus-window, ~/.agents/skills/portus-window, and ~/.codex/skills/portus-window.

See [docs/SKILL.md](docs/SKILL.md) for what the skill provides.

## Documentation

- [`docs/CLI.md`](docs/CLI.md): full CLI reference, targeting model, exit codes.
- [`docs/LINUX.md`](docs/LINUX.md): Linux/X11 implementation.
- [`docs/WINDOWS.md`](docs/WINDOWS.md): Windows implementation.
- [`docs/SKILL.md`](docs/SKILL.md): the agent skill.

## License

Apache License 2.0. See [`LICENSE`](LICENSE).
