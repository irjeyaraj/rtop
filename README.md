# rtop

A lightweight terminal system monitor for Linux terminals, built with Ratatui and Crossterm. rtop provides a dashboard view of CPU, memory, and basic GPU info, a simple "top/htop" style process pane, a Services (SystemD) tab, a recursive Logs browser, a Journal tab, and an embedded Shell for quick commands.

Press F1 in the app to see a concise Help popup.

## Features
- TUI dashboard with CPU load gauges and memory usage
- Applications frame showing Apache2, Nginx, Postgresql, Mysql, Podman, Docker (Installed/Active)
- Basic GPU detection (best-effort via /sys/class/drm and optional NVIDIA proc info)
- Top tabs for quick navigation:
  - Dashboard (F2)
  - top/htop (F3) with scrollable process table and details popup (Enter)
  - Services (SystemD) (F4) with scrollable table and per-row status color; details popup (Enter)
  - Logs (F5) recursively lists /var/log with Enter-to-open; prompts for sudo password on permission denied (excludes /var/log/journal)
  - Journal (F6) lists /var/log/journal files; Enter displays entries via journalctl; prompts for sudo on permission denied
  - Shell (F12) embedded PTY shell
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
- Switch top tabs: Left/Right, h/l, Tab/BackTab, or 1/2/3/4/5/6
- In tables (Processes/Services/Logs/Journal): Home/End jump to first/last; PgUp/PgDn move selection by 10
- In Log/Journal popups: Up/Down/Left/Right scroll by 1 line, PgUp/PgDn by a page, Home/End to top/bottom; Esc or Enter to close
- Direct tab shortcuts: F2 (Dashboard), F3 (top/htop), F4 (Services), F5 (Logs), F6 (Journal), F12 (Shell)
- Context actions: Enter on Services/Processes/Logs/Journal tables opens a details/content popup; Esc or Enter closes popups
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
