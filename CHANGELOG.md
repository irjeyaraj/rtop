# Changelog

All notable changes to this project will be documented in this file.

The format is Keep a Changelog–inspired, with dates in YYYY-MM-DD.

## [0.1.4] - 2025-08-16
- Feature: Embedded Shell tab implemented using a PTY; the shell runs inside the Shell tab.
- Input: While in Shell, most keys are forwarded to the shell; Ctrl-C goes to the shell (F10 still exits the app).
- UX: PTY resizes on terminal resize; leaving the Shell tab terminates the session to avoid leaks.
- Cleanup: Removed unused legacy run_system_shell() and related code; cleared compiler warnings.
- Docs: README and in-app Help updated for embedded shell behavior.

## [0.1.3] - 2025-08-16
- Optimization: Cache GPU detection at startup instead of every frame.
- Docs: Updated README Credits and Help remain accurate.
- Misc: Minor internal cleanups.

## [0.1.2] - 2025-08-16
- Shell: Pressing F12 now temporarily suspends the TUI and opens your system shell in the current terminal. Exit the shell to return to rtop. The previous embedded PTY view was removed to simplify behavior.
- Navigation: Added Vim-style h/l keys for switching tabs (disabled while in shell so you can type normally).
- Navigation: Removed mouse bindings and mouse capture for navigation.
- Documentation: Updated in-app Help (F1) and README to reflect the above behavior.
- Dependencies: Updated to sysinfo 0.37.0, crossterm 0.29.0.
- Build: Copyright year is read from Cargo.toml metadata at build time and shown in the Help popup.

## [0.1.1] - 2025-07-XX
- Various UI refinements and internal cleanups.

## [0.1.0] - 2025-07-XX
- Initial release with dashboard, top/htop-like process pane, and shell tab.

[0.1.2]: https://example.com/rtop/releases/0.1.2
