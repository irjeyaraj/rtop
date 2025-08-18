# Changelog

All notable changes to this project will be documented in this file.

The format is Keep a Changelog–inspired, with dates in YYYY-MM-DD.

## [0.1.6] - 2025-08-18
- Clean/Optimize: Minor code tidying; ensured zero warnings on build; small internal refactors only.
- Docs: Updated README to clarify table navigation and note journal exclusion in Logs.
- Meta: Bumped version to 0.1.6.

## [0.1.5] - 2025-08-18
- Fix: Removed unused functions (run_command_capture, draw_command_prompt, draw_command_result_popup) to eliminate compiler warnings.
- Fix: Removed unnecessary mutable variables and dead code warnings.
- Logs: Exclude /var/log/journal from Logs tab listing.
- Shell: Added a one-line system prompt banner in the Shell tab for context.
- Docs: Updated in-app Help and README to reflect current navigation and table keybindings.
- Meta: Bumped version to 0.1.5.

## [0.1.4] - 2025-08-16
- Feature: Embedded Shell tab implemented using a PTY; the shell runs inside the Shell tab.
- Input: While in Shell, most keys are forwarded to the shell; Ctrl-C goes to the shell (F10 still exits the app).
- UX: PTY resizes on terminal resize; leaving the Shell tab terminates the session to avoid leaks.
- Services: Added “Services (SystemD)” tab (F4) with a scrollable table of all services, colored status, and Enter-to-view details popup.
- Processes: top/htop tab now shows a scrollable, selectable table with Enter-to-view details popup.
- Logs: Added Logs tab (F5). Recursively lists /var/log in a scrollable table; Enter opens a popup with contents. If permission is denied, a sudo password prompt is shown and used to read the file.
- Dashboard: Added Applications frame (Apache2, Nginx, Postgresql, Mysql, Podman, Docker) with Active/Installed detection; rearranged with GPU panel.
- Cleanup: Fixed an unused import warning by simplifying fmt_system_time; minor refactors and bounds checks.
- Docs: Updated in-app Help and README to reflect new tabs and hotkeys.

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

