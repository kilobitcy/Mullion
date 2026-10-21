# Mullion

**A native, GPU-accelerated SSH client for Windows — built so that driving Claude Code inside a remote tmux session over a high-latency link feels as smooth as running it locally.**

Single native binary. No Electron, no webview, no runtime dependencies. Written in Rust.

[中文说明](README.zh.md)

---

## Why

Existing tools each solve half of the problem:

- **Terminal emulators** (Windows Terminal, WezTerm, Alacritty) have great emulation, but no file management, and they aren't designed for high-latency remote work.
- **All-in-one SSH clients** (MobaXterm, FinalShell, Xshell, Termius) have great file management, but uneven support for modern terminal protocols — full-screen TUIs, mouse capture, synchronized output.

Claude Code's full-screen TUI stresses both at once: alternate screen + mouse capture, wrapped in tmux, over a cross-border proxy. Mullion targets exactly that intersection.

## Features

**Terminal**
- xterm-compatible emulation (256 colour / truecolor / alternate screen) on top of `alacritty_terminal`, with a custom wgpu + glyphon renderer
- **Synchronized output (DEC 2026)** — frames are held between BSU/ESU and presented once, eliminating flicker during streaming output; includes registering the `sync` feature with tmux so it actually works *through* tmux
- Frame-capped redraw, decoupled from data ingestion; the event loop idles at zero CPU
- Kitty keyboard protocol with graceful fallback; correct **Shift+Enter** encoding for Claude Code
- SGR mouse reporting, with **Shift** as an escape hatch for local selection and copy
- CJK wide characters and font fallback; IME input that survives focus changes
- Bracketed paste and configurable scrollback

**Split panes & tabs**
- Arbitrarily nested horizontal/vertical splits, drag-to-resize, geometric focus navigation
- Every layout change immediately sends `window_change`, so remote TUIs reflow correctly
- New panes open a new channel on the **same SSH connection** — no extra handshakes

**Connections**
- Password, public key and ssh-agent authentication; `~/.ssh/config` import
- TOFU host-key verification with an explicit prompt on key change
- SOCKS5 / HTTP CONNECT proxies (with auth) and ProxyJump, including jump hosts behind a proxy
- Actionable error messages (DNS vs. refused vs. auth failure vs. host-key change)
- Bounded exponential-backoff reconnect; never retries automatically on a host-key change
- Latency tuning for proxied links: 30 s keepalive, 8 MB SSH window, bounded outbound queue

**Files (SFTP)**
- Dual-pane remote/local file browser with drag-and-drop between panes
- Drag-and-drop upload from Windows Explorer, and **drag-out to Explorer** via virtual files (no temp copies)
- Edit remote files in place with your default editor or the built-in one; changes upload automatically
- Global transfer queue with progress, cancellation and conflict handling

**Sessions, automation & tunnels**
- Session manager with groups, inherited settings, colours and icons
- Post-login automation: tmux auto-attach (attach-or-create), startup commands, working directory, environment variables
- Port forwarding (`-L`, `-R`, `-D`/SOCKS5) as first-class objects that run independently of any terminal window
- Encrypted credential vault (XChaCha20-Poly1305, optional Argon2id master password); reusable credentials shared across sessions
- Optional encrypted backup to **your own** S3-compatible object storage — Mullion runs no server and holds no user data
- No telemetry

## Architecture

A Cargo workspace with a strictly one-way dependency graph:

```
mullion-core    Layout tree (split panes). No UI, no IO, no async — fully unit-testable.
mullion-term    VT emulation wrapper + keyboard/mouse encoding.
mullion-ssh     russh-based transport. Knows byte streams, not panes or windows.
mullion-store   Session/credential persistence. TOML + encrypted secrets. Sync IO only.
mullion-cloud   S3-compatible object-storage client. Bytes and keys only.
mullion-app     winit + wgpu + glyphon (terminal) + egui (chrome). The only crate that knows the others.
```

`app → {core, term, ssh, store, cloud}`; nothing else depends on anything else. The payoff: layout bugs and key-encoding bugs — the most time-consuming class of terminal bugs — can be reproduced in tests **without opening a window**.

Key design decisions are recorded as ADRs in [`docs/`](docs/) (glyph rendering, config format, async boundary, crypto backend, one-connection-many-channels, tunnels as first-class objects, and more).

### Library choices

| Choice | Why | Instead of |
|---|---|---|
| `alacritty_terminal` | Apache-2.0; VT state machine only, no rendering | `wezterm-term` (weaker API stability and docs) |
| `glyphon` | cosmic-text shaping + wgpu glyph atlas, with CJK font fallback | hand-written glyph atlas |
| `russh` (ring backend) | Pure Rust — no libssh2/C toolchain, clean cross-compilation to Windows | `ssh2` (links C libraries) |
| bare `winit` + `wgpu` | Split panes are the core feature, so they're built in-house | `iced` (its `pane_grid` would replace the core) |

## Quality

- **3,500+ automated tests**, including VT byte-stream fixtures recorded from real tmux sessions
- Each known terminal pitfall (missing PTY write-back, unbatched sync frames, IME being disabled by the UI toolkit, selections wiped by repaints, …) has a dedicated guard test
- Performance budgets: idle CPU < 1 % of one core, redraw rate ≤ display refresh, resident memory < 300 MB with 8 panes and 10,000 lines of scrollback

## Install

Download `mullion.exe` from [Releases](../../releases) — a single executable. Verify it with the accompanying `mullion.exe.sha256`. It is Authenticode-signed with a **self-signed** certificate (`mullion-codesign.cer`, also attached to the release), so Windows will show an unknown publisher unless you import that certificate.

Windows 11 is the primary platform. macOS and Linux builds compile but aren't supported targets.

## Build from source

```bash
cargo test -p mullion-core    # layout logic, no GPU needed
cargo test -p mullion-term    # key encoding + VT snapshots
cargo test --workspace
cargo run  -p mullion-app
```

Cross-compiling to Windows from Linux is documented in [`docs/cross-compile-windows.md`](docs/cross-compile-windows.md).

## Scope

Mullion deliberately does **not** do: terminal multiplexing (that's tmux's job), a built-in AI sidebar, database clients, RDP/VNC or serial connections, user accounts, or mobile.

## License

Apache-2.0
