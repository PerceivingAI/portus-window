# Portus Window Skill

The Portus Window **skill** is a packaged capability file that makes an AI agent aware of Portus Window: what it does and how to control it. It is the bridge between the application and the agent.

## What the skill is

The skill is a machine-readable description of Portus Window (front matter plus a command reference) that an agent runtime loads. When installed, the agent knows:

- Portus Window exists on the machine and can show web pages, media, screenshots, and windows on its behalf.
- What Portus Window does not do, so it stops reaching for a browser or a filesystem when the wrong tool.
- The exact commands, arguments, targeting rules, and exit codes to call `portus-window-cli` correctly on the first attempt.

Without the skill, the agent may not know the app exists. With it, showing content to the user is a first-class agent action.

## Why it exists

Portus Window exists so an agent can show things to the user: a page, a chart, a video, a document, and then verify what was shown. The skill packages that into four abilities:

- **present**: open web pages, local media, or managed YouTube in real desktop windows;
- **verify**: screenshots, live titles/URLs/console state, video recordings;
- **interact**: typed DOM actions (fill forms, click, wait, assert) inside windows it opened;
- **arrange**: apply and export whole multi-window layouts, move windows across desktops, control playback, one window or all.

## What the skill contains

The skill file (`SKILL.md`, installed under `skills/portus-window/`) includes:

- **Platform and transport**: Unix socket `/tmp/portus-window.socket` on Linux, named pipe `\\.\pipe\portus-window` on Windows, protocol v7, newline-delimited JSON frames bounded at 64 KB.
- **Full command reference**: `ping`, `open`, `apply`/`export`, `interact`, `media`, `screenshot`/`record`, bulk `--all` operations, `history`/`config`, `workspaces`, `auth-session`.
- **Targeting model**: the `wsess_<32-hex>` identity, selector precedence (exact ID, description tag, title, URL), exact IDs never falling through to fuzzy matching.
- **Workspace indexing**: 1-based on Windows, 0-based on Linux/X11. Use `workspaces` to discover.
- **Exit codes**: eight typed codes (`0` success through `7` internal error) for deterministic failure handling.
- **Security boundaries**: HTTP/HTTPS-only web content, no `file://`, no arbitrary JavaScript, no popups, Firefox-only authenticated sessions under explicit user consent, zero secret exposure.

## Installing the skill

```bash
./install_skill.sh
```

This copies `skills/portus-window/SKILL.md` into the agent's skill directory. After that the agent can use Portus Window immediately:

```bash
portus-window-cli ping
portus-window-cli open "https://example.com" --wait-loaded
portus-window-cli screenshot --all --out /tmp/captures/
```

## Scope

The skill teaches the agent to use Portus Window. Nothing more. It grants no system authority; the host process enforces all security boundaries. Full browser automation, cross-origin traversal, and native-input automation belong in a dedicated tool such as Playwright.

See [`CLI.md`](CLI.md) for the complete command reference, [`LINUX.md`](LINUX.md) and [`WINDOWS.md`](WINDOWS.md) for platform details.
