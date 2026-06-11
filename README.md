# AoE4 Overlay (Rust / Tauri)

A Rust/Tauri rewrite of [FluffyMaguro/AoE4_Overlay](https://github.com/FluffyMaguro/AoE4_Overlay).

Shows a transparent, click-through, always-on-top in-game overlay with live player
stats (rating, rank, winrate, civ winrate) for your current Age of Empires IV match,
powered by the [aoe4world.com](https://aoe4world.com) API.

## Features

- **Control panel** — search for your aoe4world profile, configure polling interval,
  hotkey, and auto-show behavior; see the last match table.
- **Overlay window** — transparent, frameless, click-through, always-on-top.
  Shows both teams with civ flags, rating (blue), winrate (yellow), wins/losses
  (green/red), and per-civ winrate (purple).
- **Global hotkey** (default `Alt+O`) to show/hide the overlay.
- **Reposition mode** — click "Reposition overlay" in the control panel, drag the
  overlay where you want it; the position is saved.
- **Auto-detection** — polls `https://aoe4world.com/api/v0/players/{id}/games/last`
  (default every 15 s) and pushes new-match data to the overlay automatically.

## Build & run

Requires Rust (stable) and the WebView2 runtime (preinstalled on Windows 11).

```
cargo run --manifest-path src-tauri/Cargo.toml          # dev
cargo build --release --manifest-path src-tauri/Cargo.toml
```

Settings persist to `%APPDATA%\com.georgewall.aoe4overlay\config.json`.

## Architecture

- `src-tauri/src/api.rs` — aoe4world API client + `process_game` (raw match JSON →
  overlay payload, mirroring the original Python `process_game`).
- `src-tauri/src/lib.rs` — Tauri setup, app state, poller task, commands, global hotkey.
- `src-tauri/src/settings.rs` — JSON-persisted settings.
- `ui/index.html` — control panel (vanilla JS, Tauri global API).
- `ui/overlay.html` — the overlay itself; receives `game_data` Tauri events
  (replaces the original's local WebSocket server).
- `ui/flags/` — civ flag images from the original project.

Feature parity with the original: match overlay, build-order overlay
(`ui/buildorder.html`, RTS_Overlay JSON + plain text, @image@ tokens, 4 global
hotkeys: show/cycle/prev/next step), match history tab, caster override tab,
civ/map randomizer, team colors / civ-stats color / font scale settings, and a
local websocket server (default port 7307) so `obs-html\overlay.html` can be
used as an OBS browser source. Not ported: rating graphs (disabled upstream too).
