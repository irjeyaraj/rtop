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
- Exit: F10, or press `q` or `Ctrl-C`

Note: F12 temporarily suspends the TUI and runs your default shell in this terminal. Exit the shell to return to rtop. Vim-style `h`/`l` navigation is disabled while in shell.

## Shell
Press F12 to open your system shell directly in the current terminal. When you exit that shell, you’ll return to rtop.

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
