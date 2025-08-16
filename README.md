# rtop

A lightweight terminal system monitor for Linux terminals, built with Ratatui and Crossterm. rtop provides a dashboard view of CPU, memory, and basic GPU info, a simple "top/htop" style process pane, and a Shell tab for quick commands.

Press F1 in the app to see a concise Help popup.

## Features
- TUI dashboard with CPU load gauges and memory usage
- Basic GPU detection (best-effort via /sys/class/drm and optional NVIDIA proc info)
- Top tabs for quick navigation:
  - Dashboard (F2)
  - top/htop (F3)
  - Shell (F12)
- Keyboard-driven navigation; runs in a standard terminal

## Installation
You need Rust (cargo) installed. On Linux, run:

```
cargo build --release
```

The binary will be at `target/release/rtop`.

## Usage
Run the executable in your terminal:

```
./target/release/rtop
```

Press F1 at any time to bring up in-app help.

### Controls (summary)
- Switch top tabs: Left/Right, h/l, Tab/BackTab, or 1/2/3
- Quick navigation: Home (Dashboard), End (last tab), PgDn (previous tab), PgUp (next tab)
- Direct tab shortcuts: F2 (Dashboard), F3 (top/htop), F12 (Shell)
- Exit: F10, or press `q`

Note: F12 opens an embedded shell (PTY) inside the Shell tab. While on the Shell tab, most keys are forwarded to your shell. Ctrl-C is sent to the shell (it will not quit rtop). Use F10 to exit the app. Vim-style `h`/`l` navigation is disabled while in shell so you can type normally.

## Shell
Press F12 to switch to the Shell tab and use your system shell embedded within rtop. When you leave the Shell tab or exit the shell process, you’ll return to the rest of rtop. If the shell exits, press F12 again to start a new session.

## Platform support
rtop targets Linux. Some features (like GPU detection/temperature) are best-effort and depend on available sysfs/proc files and drivers.

## License
GPL-2.0-or-later. See [LICENSE](LICENSE).

## Credits
- [ratatui](https://github.com/ratatui-org/ratatui)
- [crossterm](https://github.com/crossterm-rs/crossterm)
- [sysinfo](https://github.com/GuillaumeGomez/sysinfo)

## Changelog
See [CHANGELOG.md](CHANGELOG.md).
