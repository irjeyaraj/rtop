// Copyright (c) 2025 Immanuel Raja Jeyaraj <irj@sefier.com>
//! rtop: a lightweight terminal system monitor.
//! See the in-app Help (F1) for usage and hotkeys.
use std::error::Error;
use std::io;
use std::time::{Duration, Instant};
use std::process::Command;
use std::process::Stdio;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Gauge, Block, Borders, Clear, Table, Row, Cell};
use ratatui::Terminal;
use sysinfo::{CpuRefreshKind, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};
use std::fs;

mod shell;
mod app;
use app::App;
use shell::ShellSession;




/// Program entry point: sets up the terminal backend, runs the app loop,
/// and restores the terminal state on exit.
fn main() -> Result<(), Box<dyn Error>> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::cursor::Hide
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Ensure we restore terminal on exit
    let res = run_app(&mut terminal);

    // Restore terminal
    disable_raw_mode().ok();
    let mut out = io::stdout();
    crossterm::execute!(
        out,
        crossterm::cursor::Show,
        crossterm::terminal::LeaveAlternateScreen
    )
    .ok();

    if let Err(e) = res {
        eprintln!("rtop error: {}", e);
        std::process::exit(1);
    }
    Ok(())
}


/// Main application loop: handles periodic refresh, input events, and drawing.
fn run_app(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<(), Box<dyn Error>> {
    let mut app = App::default();
    // Cache GPU detection once at startup
    app.gpus = detect_gpus();

    // Prepare sysinfo system with specific refresh kinds to be efficient
    let refresh = RefreshKind::nothing()
        .with_cpu(CpuRefreshKind::everything())
        .with_processes(ProcessRefreshKind::everything());
    let mut sys = System::new_with_specifics(refresh);

    let tick_rate = Duration::from_millis(800);
    let mut last_tick = Instant::now();

    // Note: sysinfo 0.30 does not expose refresh_users_list; users info will be read from sys.users()
    // Initialize network prev counters once before entering loop
    #[cfg(target_os = "linux")]
    {
        for (iface, rx, tx) in read_network_counters() {
            app.net_prev.insert(iface, (rx, tx));
        }
        app.net_last = Instant::now();
    }
    loop {
        // Update network rates (Linux)
        #[cfg(target_os = "linux")]
        {
            let now = Instant::now();
            let dt = now.saturating_duration_since(app.net_last).as_secs_f64();
            if dt > 0.0 {
                let mut new_prev = app.net_prev.clone();
                for (iface, rx, tx) in read_network_counters() {
                    if let Some((prx, ptx)) = app.net_prev.get(&iface).cloned() {
                        let drx = rx.saturating_sub(prx) as f64;
                        let dtx = tx.saturating_sub(ptx) as f64;
                        app.net_rates.insert(iface.clone(), (drx / dt, dtx / dt));
                    } else {
                        // No previous data; show 0 for first update
                        app.net_rates.insert(iface.clone(), (0.0, 0.0));
                    }
                    new_prev.insert(iface, (rx, tx));
                }
                app.net_prev = new_prev;
                app.net_last = now;
            }
        }

        // Refresh data periodically
        sys.refresh_cpu_all();
        sys.refresh_processes(ProcessesToUpdate::All, true);
        sys.refresh_memory();

        // Build processes cache: PIDs sorted by CPU% (descending)
        {
            let mut pairs: Vec<(i32, f32)> = sys
                .processes()
                .iter()
                .map(|(pid, p)| (pid.as_u32() as i32, p.cpu_usage()))
                .collect();
            pairs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            app.procs_pids_sorted = pairs.into_iter().map(|(pid, _)| pid).collect();
            // Clamp selection to available items
            if !app.procs_pids_sorted.is_empty() {
                let max_idx = app.procs_pids_sorted.len().saturating_sub(1);
                if app.procs_selected > max_idx { app.procs_selected = max_idx; }
            } else {
                app.procs_selected = 0;
                app.procs_scroll = 0;
            }
        }

        // Ensure shell session lifecycle and sizing
        if app.selected_top_tab == 3 {
            let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
            let rows = rows.saturating_sub(3); // account for menu and borders
            let cols = cols.saturating_sub(2);
            let rows = rows.max(1);
            let cols = cols.max(1);
            if app.shell.is_none() {
                app.shell = ShellSession::spawn(rows, cols);
            }
            if let Some(sess) = app.shell.as_mut() {
                // If shell exited (logout), return to Dashboard
                if sess.is_exited() {
                    let _ = app.shell.take();
                    app.selected_top_tab = 0;
                } else {
                    sess.resize(rows, cols);
                }
            }
        }

        // Draw UI
        terminal.draw(|f| {
            let size = f.area();

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(3),    // main content
                    Constraint::Length(1), // menu bar (function keys)
                ])
                .split(size);

            draw_header(f, chunks[0], &sys, &app);
            draw_menu(f, chunks[1], &app);

            // Overlays (drawn last, on top)
            if app.help_popup {
                draw_help_popup(f, size);
            }
            if app.service_popup {
                draw_service_popup(f, size, &app.service_detail_title, &app.service_detail_text);
            }
            if app.process_popup {
                draw_process_popup(f, size, &app.process_detail_title, &app.process_detail_text);
            }
            if app.log_popup {
                draw_log_popup(f, size, &app.log_detail_title, &app.log_detail_text);
            }
            if app.logs_password_prompt {
                draw_logs_password_prompt(f, size, &app.logs_password_error, app.logs_password_input.chars().count());
            }
        })?;


        // Handle input with non-blocking poll, but ensure a minimum tick rate
        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_millis(0));

        if event::poll(timeout)? {
            match event::read()? {
                Event::Key(key) => {
                    if handle_key(key, &mut app)? { break; }
                }
                Event::Resize(_, _) => {
                    if app.selected_top_tab == 3 {
                        if let Some(sess) = app.shell.as_mut() {
                            let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
                            let rows = rows.saturating_sub(3); // account for menu and borders
                            let cols = cols.saturating_sub(2);
                            let rows = rows.max(1);
                            let cols = cols.max(1);
                            sess.resize(rows, cols);
                        }
                    }
                }
                _ => {}
            }
        }
        if last_tick.elapsed() >= tick_rate {
            last_tick = Instant::now();
        }
    }

    Ok(())
}

/// Handle a single key event.
///
/// Returns Ok(true) to request application exit.
fn handle_key(key: KeyEvent, app: &mut App) -> Result<bool, Box<dyn Error>> {
    // Close help overlay on Esc
    if app.help_popup {
        if let KeyCode::Esc = key.code {
            app.help_popup = false;
            return Ok(false);
        }
    }
    // Close service details popup on Esc or Enter
    if app.service_popup {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => {
                app.service_popup = false;
                return Ok(false);
            }
            _ => {}
        }
    }
    // Close process details popup on Esc or Enter
    if app.process_popup {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => {
                app.process_popup = false;
                return Ok(false);
            }
            _ => {}
        }
    }
    // Close log content popup on Esc or Enter
    if app.log_popup {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => {
                app.log_popup = false;
                return Ok(false);
            }
            _ => {}
        }
    }
    // Handle sudo password prompt input
    if app.logs_password_prompt {
        match key.code {
            KeyCode::Esc => {
                app.logs_password_prompt = false;
                app.logs_password_input.clear();
                app.logs_password_error.clear();
                return Ok(false);
            }
            KeyCode::Enter => {
                if app.logs_password_input.is_empty() {
                    app.logs_password_error = "Password cannot be empty".to_string();
                } else {
                    app.logs_sudo_password = Some(app.logs_password_input.clone());
                    app.logs_password_input.clear();
                    app.logs_password_error.clear();
                    app.logs_password_prompt = false;
                    // If we have a pending path, attempt to read now and show
                    if !app.logs_pending_path.is_empty() {
                        let path = app.logs_pending_path.clone();
                        let title = std::path::Path::new(&path).file_name().and_then(|s| s.to_str()).unwrap_or(&path).to_string();
                        match read_log_file_best_effort(&path, app.logs_sudo_password.as_deref()) {
                            Ok(text) => { app.log_detail_title = title; app.log_detail_text = text; app.log_popup = true; }
                            Err(err) => { app.log_detail_title = title; app.log_detail_text = format!("Failed to read: {}", err); app.log_popup = true; }
                        }
                        app.logs_pending_path.clear();
                    }
                }
                return Ok(false);
            }
            KeyCode::Backspace => {
                app.logs_password_input.pop();
                return Ok(false);
            }
            KeyCode::Char(c) => {
                // basic acceptance of printable chars
                if !c.is_control() {
                    app.logs_password_input.push(c);
                }
                return Ok(false);
            }
            _ => {}
        }
    }



    // If Shell tab active, forward most keys to the PTY instead of handling as app hotkeys
    if app.selected_top_tab == 3 {
        // Ensure shell session exists
        if app.shell.is_none() {
            app.shell = ShellSession::spawn(24, 80);
        }
        if let Some(sess) = app.shell.as_mut() {
            // Allow a couple of app-level keys
            match (key.code, key.modifiers) {
                (KeyCode::F(10), _) => return Ok(true), // exit app
                (KeyCode::F(1), _) => { app.help_popup = !app.help_popup; return Ok(false); }
                _ => {}
            }
            // Forward key to shell
            match (key.code, key.modifiers) {
                (KeyCode::Char(c), KeyModifiers::CONTROL) => {
                    let uc = c.to_ascii_uppercase();
                    if uc >= 'A' && uc <= 'Z' {
                        let b = (uc as u8 - b'@') as u8;
                        sess.write_bytes(&[b]);
                    }
                }
                (KeyCode::Char(c), _) => {
                    let s = c.to_string();
                    sess.write_bytes(s.as_bytes());
                }
                (KeyCode::Enter, _) => sess.write_bytes(&[b'\n']),
                (KeyCode::Backspace, _) => sess.write_bytes(&[0x7f]),
                (KeyCode::Tab, _) => sess.write_bytes(&[b'\t']),
                (KeyCode::Esc, _) => sess.write_bytes(&[0x1b]),
                (KeyCode::Left, _) => sess.write_bytes(b"\x1b[D"),
                (KeyCode::Right, _) => sess.write_bytes(b"\x1b[C"),
                (KeyCode::Up, _) => sess.write_bytes(b"\x1b[A"),
                (KeyCode::Down, _) => sess.write_bytes(b"\x1b[B"),
                (KeyCode::Home, _) => sess.write_bytes(b"\x1b[H"),
                (KeyCode::End, _) => sess.write_bytes(b"\x1b[F"),
                (KeyCode::PageUp, _) => sess.write_bytes(b"\x1b[5~"),
                (KeyCode::PageDown, _) => sess.write_bytes(b"\x1b[6~"),
                (KeyCode::Delete, _) => sess.write_bytes(b"\x1b[3~"),
                (KeyCode::Insert, _) => sess.write_bytes(b"\x1b[2~"),
                _ => {}
            }
            return Ok(false);
        }
    }

    // Selection and actions for top/htop Processes table
    if app.selected_top_tab == 1 {
        match key.code {
            KeyCode::Up => {
                if app.procs_selected > 0 { app.procs_selected -= 1; }
                return Ok(false);
            }
            KeyCode::Down => {
                app.procs_selected = app.procs_selected.saturating_add(1);
                return Ok(false);
            }
            KeyCode::Home => {
                app.procs_selected = 0;
                return Ok(false);
            }
            KeyCode::End => {
                if !app.procs_pids_sorted.is_empty() {
                    app.procs_selected = app.procs_pids_sorted.len().saturating_sub(1);
                }
                return Ok(false);
            }
            KeyCode::PageUp => {
                let step: usize = 10;
                app.procs_selected = app.procs_selected.saturating_sub(step);
                return Ok(false);
            }
            KeyCode::PageDown => {
                let step: usize = 10;
                app.procs_selected = app.procs_selected.saturating_add(step);
                return Ok(false);
            }
            KeyCode::Enter => {
                if !app.procs_pids_sorted.is_empty() {
                    let idx = app.procs_selected.min(app.procs_pids_sorted.len().saturating_sub(1));
                    let pid = app.procs_pids_sorted[idx];
                    let title = get_process_name(pid);
                    let text = get_process_details(pid);
                    app.process_detail_title = if title.is_empty() { format!("PID {}", pid) } else { format!("{} (PID {})", title, pid) };
                    app.process_detail_text = text;
                    app.process_popup = true;
                }
                return Ok(false);
            }
            _ => {}
        }
    }

    // Selection and actions for Services tab
    if app.selected_top_tab == 2 {
        match key.code {
            KeyCode::Up => {
                if app.services_selected > 0 { app.services_selected -= 1; }
                return Ok(false);
            }
            KeyCode::Down => {
                // Increase selection but cap to last item based on current listing (best effort)
                #[cfg(target_os = "linux")]
                {
                    let total = get_all_services().len();
                    if total == 0 { /* no-op */ }
                    else {
                        let max_idx = total.saturating_sub(1);
                        if app.services_selected < max_idx { app.services_selected += 1; }
                    }
                }
                #[cfg(not(target_os = "linux"))]
                {
                    // nothing to select on non-linux view
                }
                return Ok(false);
            }
            KeyCode::Home => {
                app.services_selected = 0;
                return Ok(false);
            }
            KeyCode::End => {
                #[cfg(target_os = "linux")]
                {
                    let total = get_all_services().len();
                    if total > 0 { app.services_selected = total.saturating_sub(1); }
                }
                return Ok(false);
            }
            KeyCode::PageUp => {
                let step: usize = 10;
                app.services_selected = app.services_selected.saturating_sub(step);
                return Ok(false);
            }
            KeyCode::PageDown => {
                let step: usize = 10;
                app.services_selected = app.services_selected.saturating_add(step);
                #[cfg(target_os = "linux")]
                {
                    let total = get_all_services().len();
                    if total > 0 {
                        let max_idx = total.saturating_sub(1);
                        if app.services_selected > max_idx { app.services_selected = max_idx; }
                    }
                }
                return Ok(false);
            }
            KeyCode::Enter => {
                // Open popup with selected service details
                #[cfg(target_os = "linux")]
                {
                    let services = get_all_services();
                    if !services.is_empty() {
                        let idx = app.services_selected.min(services.len().saturating_sub(1));
                        let (unit, _active, _desc) = &services[idx];
                        app.service_detail_title = unit.clone();
                        app.service_detail_text = get_service_status(unit);
                        app.service_popup = true;
                    }
                }
                #[cfg(not(target_os = "linux"))]
                {
                    app.service_detail_title = String::from("N/A");
                    app.service_detail_text = String::from("Service details are supported on Linux only.");
                    app.service_popup = true;
                }
                return Ok(false);
            }
            _ => {}
        }
    }

    // Selection and actions for Logs tab
    if app.selected_top_tab == 4 {
        match key.code {
            KeyCode::Up => { if app.logs_selected > 0 { app.logs_selected -= 1; } return Ok(false); }
            KeyCode::Down => { app.logs_selected = app.logs_selected.saturating_add(1); return Ok(false); }
            KeyCode::Home => { app.logs_selected = 0; return Ok(false); }
            KeyCode::End => {
                let total = list_var_log_files().len();
                if total > 0 { app.logs_selected = total.saturating_sub(1); }
                return Ok(false);
            }
            KeyCode::PageUp => { let step: usize = 10; app.logs_selected = app.logs_selected.saturating_sub(step); return Ok(false); }
            KeyCode::PageDown => {
                let step: usize = 10; app.logs_selected = app.logs_selected.saturating_add(step);
                let total = list_var_log_files().len();
                if total > 0 {
                    let max_idx = total.saturating_sub(1);
                    if app.logs_selected > max_idx { app.logs_selected = max_idx; }
                }
                return Ok(false);
            }
            KeyCode::Enter => {
                // Build file list and attempt to read selected file
                let files = list_var_log_files();
                if !files.is_empty() {
                    let idx = app.logs_selected.min(files.len().saturating_sub(1));
                    let ent = &files[idx];
                    // Try normal read first, then sudo if we have password; if denied and no password, prompt
                    match read_log_file_best_effort(&ent.path, app.logs_sudo_password.as_deref()) {
                        Ok(text) => {
                            app.log_detail_title = ent.name.clone();
                            app.log_detail_text = text;
                            app.log_popup = true;
                        }
                        Err(err) => {
                            // If error suggests permission denied and no password yet, open prompt
                            // We cannot reliably parse OS error text; if we have no password, prompt anyway
                            if app.logs_sudo_password.is_none() {
                                app.logs_pending_path = ent.path.clone();
                                app.logs_password_prompt = true;
                                app.logs_password_error.clear();
                            } else {
                                // Show error in popup
                                app.log_detail_title = ent.name.clone();
                                app.log_detail_text = format!("Failed to read: {}", err);
                                app.log_popup = true;
                            }
                        }
                    }
                }
                return Ok(false);
            }
            _ => {}
        }
    }

    match (key.code, key.modifiers) {
        (KeyCode::Char('q'), _) => return Ok(true),
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => return Ok(true),
        // Function keys hotkeys
        (KeyCode::F(10), _) => return Ok(true), // F10 exit
        (KeyCode::F(9), _) => { /* intentionally unmapped */ }
        (KeyCode::F(1), _) => { app.help_popup = !app.help_popup; } // F1 Help popup
        (KeyCode::F(2), _) => { app.selected_top_tab = 0; } // F2 Dashboard
        (KeyCode::F(3), _) => { app.selected_top_tab = 1; } // F3 top/htop
        (KeyCode::F(4), _) => { app.selected_top_tab = 2; } // F4 Services (SystemD)
        (KeyCode::F(5), _) => { app.selected_top_tab = 4; } // F5 Logs
        // F6 intentionally left unmapped
        (KeyCode::F(11), _) => { /* intentionally unmapped */ }
        (KeyCode::F(12), _) => { app.selected_top_tab = 3; } // F12 Shell tab
        // Top tabs navigation (Left/Right, Tab/BackTab, number keys)
        (KeyCode::Left, _) => {
            if app.selected_top_tab > 0 { app.selected_top_tab -= 1; }
        }
        (KeyCode::Right, _) => {
            if app.selected_top_tab < 4 { app.selected_top_tab += 1; }
        }
        // Vim-style: h = left, l = right
        (KeyCode::Char('h'), _) => {
            if app.selected_top_tab > 0 { app.selected_top_tab -= 1; }
        }
        (KeyCode::Char('H'), _) => {
            if app.selected_top_tab > 0 { app.selected_top_tab -= 1; }
        }
        (KeyCode::Char('l'), _) => {
            if app.selected_top_tab < 4 { app.selected_top_tab += 1; }
        }
        (KeyCode::Char('L'), _) => {
            if app.selected_top_tab < 4 { app.selected_top_tab += 1; }
        }
        (KeyCode::Tab, _) => {
            app.selected_top_tab = (app.selected_top_tab + 1) % 5;
        }
        (KeyCode::BackTab, _) => {
            app.selected_top_tab = (app.selected_top_tab + 4) % 5; // -1 mod 5
        }
        (KeyCode::Char('1'), _) => { app.selected_top_tab = 0; }
        (KeyCode::Char('2'), _) => { app.selected_top_tab = 1; }
        (KeyCode::Char('3'), _) => { app.selected_top_tab = 2; }
        (KeyCode::Char('4'), _) => { app.selected_top_tab = 3; }
        (KeyCode::Char('5'), _) => { app.selected_top_tab = 4; }
        _ => {}
    }

    // If we just left the Shell tab, terminate the session
    if app.selected_top_tab != 3 {
        if let Some(mut sess) = app.shell.take() { sess.terminate(); }
    }

    Ok(false)
}


// GPU detection helpers (Linux, best-effort via /sys/class/drm and /proc)
/// Basic GPU information detected from the system (Linux best-effort).
#[derive(Debug, Clone)]
struct GpuInfo {
    vendor: String,
    driver: String,
    pci_addr: String,
    model: String,
    temp_c: Option<f32>,
}

/// Map a PCI vendor ID (hex string) to a human-readable vendor name.
fn map_vendor(vendor_id: &str) -> String {
    match vendor_id.to_ascii_lowercase().as_str() {
        "0x10de" => "NVIDIA".to_string(),
        "0x1002" | "0x1022" | "0x1025" => "AMD".to_string(), // common AMD/ATI ids
        "0x8086" => "Intel".to_string(),
        other => other.to_string(),
    }
}

/// Detect GPUs from the Linux filesystem (DRM, PCI, and optional NVIDIA proc info).
fn detect_gpus() -> Vec<GpuInfo> {
    use std::fs;
    use std::path::Path;
    let mut gpus: Vec<GpuInfo> = Vec::new();
    let drm_path = Path::new("/sys/class/drm");
    if !drm_path.exists() {
        return gpus;
    }
    let Ok(entries) = fs::read_dir(drm_path) else { return gpus; };
    let mut seen_cards = Vec::new();
    for ent in entries.flatten() {
        if let Some(name) = ent.file_name().to_str().map(|s| s.to_string()) {
            // Interested in primary nodes like card0, card1; skip connectors like card0-DP-1, renderD*, controlD*
            if name.starts_with("card") && name.chars().all(|c| c.is_ascii_alphanumeric()) {
                if !seen_cards.contains(&name) {
                    seen_cards.push(name);
                }
            }
        }
    }

    for card in seen_cards {
        let dev_dir = format!("/sys/class/drm/{}/device", card);
        let vendor_id = fs::read_to_string(format!("{}/vendor", dev_dir)).unwrap_or_default().trim().to_string();
        let device_id = fs::read_to_string(format!("{}/device", dev_dir)).unwrap_or_default().trim().to_string();
        let vendor_name = map_vendor(&vendor_id);
        // Determine PCI address by real path of device dir
        let pci_addr = std::fs::canonicalize(&dev_dir)
            .ok()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().to_string()))
            .unwrap_or_default();
        // Driver module name
        let driver = std::fs::read_link(format!("{}/driver", dev_dir))
            .ok()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().to_string()))
            .unwrap_or_else(|| String::from("unknown"));
        // Try to get a nice model name (best effort)
        let mut model = String::new();
        // NVIDIA specific: /proc/driver/nvidia/gpus/*/information has "Model: ..."
        if vendor_name == "NVIDIA" {
            if let Ok(nv_dirs) = fs::read_dir("/proc/driver/nvidia/gpus") {
                for d in nv_dirs.flatten() {
                    let info_path = d.path().join("information");
                    if let Ok(info) = fs::read_to_string(info_path) {
                        for line in info.lines() {
                            if let Some(rest) = line.strip_prefix("Model:") {
                                model = rest.trim().to_string();
                                break;
                            }
                        }
                    }
                    if !model.is_empty() { break; }
                }
            }
        }
        if model.is_empty() {
            // Fallback name
            model = format!("{} GPU ({})", vendor_name, device_id);
        }
        let temp_c = read_gpu_temp_from_device_sysfs(&dev_dir);
        gpus.push(GpuInfo { vendor: vendor_name, driver, pci_addr, model, temp_c });
    }
    gpus
}

#[cfg(target_os = "linux")]
fn read_gpu_temp_from_device_sysfs(dev_dir: &str) -> Option<f32> {
    use std::fs;
    use std::path::Path;
    let hwmon_root = Path::new(dev_dir).join("hwmon");
    let entries = fs::read_dir(&hwmon_root).ok()?;
    let mut temps: Vec<(String, f32)> = Vec::new();
    for ent in entries.flatten() {
        let hpath = ent.path();
        if !hpath.is_dir() { continue; }
        // optional hwmon name may indicate gpu
        let _name = fs::read_to_string(hpath.join("name")).ok().unwrap_or_default();
        if let Ok(files) = fs::read_dir(&hpath) {
            for f in files.flatten() {
                let p = f.path();
                if let Some(fname) = p.file_name().and_then(|s| s.to_str()) {
                    if fname.starts_with("temp") && fname.ends_with("_input") {
                        if let Ok(raw) = fs::read_to_string(&p) {
                            if let Ok(mut v) = raw.trim().parse::<f32>() {
                                if v > 200.0 { v = v / 1000.0; }
                                // try label alongside
                                let label = fname.replace("_input", "_label");
                                let lab = fs::read_to_string(hpath.join(label)).ok().unwrap_or_default();
                                temps.push((lab.trim().to_string(), v));
                            }
                        }
                    }
                }
            }
        }
    }
    if temps.is_empty() { return None; }
    // Prefer sensors that look like GPU edge/hotspot, otherwise take max
    let mut best: Option<f32> = None;
    for (lab, v) in &temps {
        let l = lab.to_ascii_lowercase();
        if l.contains("edge") || l.contains("gpu") || l.contains("junction") || l.contains("hotspot") {
            best = Some(best.map_or(*v, |b| b.max(*v)));
        }
    }
    if best.is_none() {
        for (_lab, v) in temps { best = Some(best.map_or(v, |b| b.max(v))); }
    }
    best
}

#[cfg(not(target_os = "linux"))]
fn read_gpu_temp_from_device_sysfs(_dev_dir: &str) -> Option<f32> { None }

// Determine default shell program and arguments for the current platform
/// Determine the default interactive shell and args on Unix.
#[cfg(unix)]
fn default_shell_and_args() -> (String, Vec<String>) {
    // Prefer $SHELL if present
    if let Ok(shell) = std::env::var("SHELL") {
        if !shell.trim().is_empty() {
            return (shell, vec!["-i".to_string(), "-l".to_string()]);
        }
    }
    // Fallback: read UID from /proc/self/status and lookup /etc/passwd
    let mut login_shell: Option<String> = None;
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        if let Some(uid_line) = status.lines().find(|l| l.starts_with("Uid:")) {
            let mut it = uid_line.split_whitespace();
            it.next(); // skip "Uid:"
            if let Some(uid0) = it.next() {
                if let Ok(passwd) = std::fs::read_to_string("/etc/passwd") {
                    for line in passwd.lines() {
                        if line.trim().is_empty() || line.starts_with('#') { continue; }
                        let parts: Vec<&str> = line.split(':').collect();
                        if parts.len() >= 7 && parts[2] == uid0 {
                            login_shell = Some(parts[6].to_string());
                            break;
                        }
                    }
                }
            }
        }
    }
    let prog = login_shell.unwrap_or_else(|| "/bin/sh".to_string());
    (prog, vec!["-i".to_string(), "-l".to_string()])
}

/// Determine the default shell on Windows (COMSPEC or cmd.exe).
#[cfg(windows)]
fn default_shell_and_args() -> (String, Vec<String>) {
    let prog = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
    (prog, Vec::new())
}




/// Helpers to read CPU temperature on Linux from sysfs (hwmon or thermal zones).
#[cfg(target_os = "linux")]
fn read_cpu_temperature_c() -> Option<f32> {
    if let Some(t) = read_hwmon_cpu_temp() { return Some(t); }
    if let Some(t) = read_thermal_zone_cpu_temp() { return Some(t); }
    None
}

#[cfg(not(target_os = "linux"))]
fn read_cpu_temperature_c() -> Option<f32> { None }

#[cfg(target_os = "linux")]
fn read_hwmon_cpu_temp() -> Option<f32> {
    let dir = match fs::read_dir("/sys/class/hwmon") { Ok(d) => d, Err(_) => return None };
    let mut temps: Vec<(String, f32)> = Vec::new();
    for entry in dir.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_dir() { continue; }
        // Read device name to help filter CPU sensors
        let name_path = path.join("name");
        let name = fs::read_to_string(&name_path).ok().map(|s| s.trim().to_string()).unwrap_or_default();
        let is_cpuish = {
            let n = name.to_ascii_lowercase();
            n.contains("coretemp") || n.contains("k10temp") || n.contains("zenpower") || n.contains("cpu") || n.contains("soc")
        };
        // Collect temp*_label and temp*_input
        let mut pairs: Vec<(String, f32)> = Vec::new();
        if let Ok(entries) = fs::read_dir(&path) {
            for e in entries.filter_map(|e| e.ok()) {
                let p = e.path();
                if let Some(fname) = p.file_name().and_then(|s| s.to_str()) {
                    if fname.starts_with("temp") && fname.ends_with("_input") {
                        let label = fname.replace("_input", "_label");
                        let label_text = fs::read_to_string(path.join(&label)).ok().unwrap_or_default();
                        if let Ok(raw) = fs::read_to_string(&p) {
                            if let Ok(mut v) = raw.trim().parse::<f32>() {
                                if v > 200.0 { v = v / 1000.0; }
                                let lab = label_text.trim().to_string();
                                let lab_lc = lab.to_ascii_lowercase();
                                let looks_cpu = lab_lc.contains("cpu") || lab_lc.contains("package") || lab_lc.contains("tctl") || lab_lc.contains("tdie") || lab_lc.contains("core");
                                if looks_cpu || is_cpuish {
                                    pairs.push((lab, v));
                                }
                            }
                        }
                    }
                }
            }
        }
        temps.extend(pairs);
    }
    if temps.is_empty() { return None; }
    temps.sort_by(|a,b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    temps.last().map(|t| t.1)
}

#[cfg(target_os = "linux")]
fn read_thermal_zone_cpu_temp() -> Option<f32> {
    let dir = match fs::read_dir("/sys/class/thermal") { Ok(d) => d, Err(_) => return None };
    let mut best: Option<f32> = None;
    for entry in dir.filter_map(|e| e.ok()) {
        let path = entry.path();
        if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
            if !name.starts_with("thermal_zone") { continue; }
        }
        let type_s = fs::read_to_string(path.join("type")).ok().unwrap_or_default();
        let type_lc = type_s.to_ascii_lowercase();
        let looks_cpu = type_lc.contains("cpu") || type_lc.contains("x86_pkg_temp") || type_lc.contains("soc") || type_lc.contains("acpitz");
        if !looks_cpu { continue; }
        if let Ok(raw) = fs::read_to_string(path.join("temp")) {
            if let Ok(mut v) = raw.trim().parse::<f32>() {
                if v > 200.0 { v = v / 1000.0; }
                best = Some(best.map_or(v, |b| b.max(v)));
            }
        }
    }
    best
}

/// Helpers to read CPU fan speed (RPM) on Linux from sysfs (hwmon).
#[cfg(target_os = "linux")]
#[allow(dead_code)]
fn read_cpu_fan_rpm() -> Option<u32> {
    let dir = match fs::read_dir("/sys/class/hwmon") { Ok(d) => d, Err(_) => return None };
    let mut rpms: Vec<u32> = Vec::new();
    for entry in dir.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_dir() { continue; }
        let name = fs::read_to_string(path.join("name")).ok().map(|s| s.trim().to_string()).unwrap_or_default();
        let is_cpuish = {
            let n = name.to_ascii_lowercase();
            n.contains("cpu") || n.contains("coretemp") || n.contains("k10temp") || n.contains("zenpower") || n.contains("soc")
        };
        if let Ok(entries) = fs::read_dir(&path) {
            for e in entries.filter_map(|e| e.ok()) {
                let p = e.path();
                if let Some(fname) = p.file_name().and_then(|s| s.to_str()) {
                    if fname.starts_with("fan") && fname.ends_with("_input") {
                        // Optional label to refine selection
                        let label = fname.replace("_input", "_label");
                        let label_text = fs::read_to_string(path.join(&label)).ok().unwrap_or_default();
                        let lab_lc = label_text.to_ascii_lowercase();
                        let looks_cpu = lab_lc.contains("cpu") || lab_lc.contains("package") || lab_lc.contains("processor") || lab_lc.contains("cpufan");
                        if !is_cpuish && !looks_cpu { continue; }
                        if let Ok(raw) = fs::read_to_string(&p) {
                            if let Ok(v) = raw.trim().parse::<u32>() {
                                if v > 0 { rpms.push(v); }
                            }
                        }
                    }
                }
            }
        }
    }
    if rpms.is_empty() { None } else { Some(*rpms.iter().max().unwrap_or(&0)) }
}

#[cfg(not(target_os = "linux"))]
#[allow(dead_code)]
fn read_cpu_fan_rpm() -> Option<u32> { None }

// -------- CPU model helper --------
fn get_processor_model_string(sys: &System) -> String {
    // Prefer sysinfo brand when available and not "Unknown"
    let brand = sys.cpus().first().map(|c| c.brand().to_string()).unwrap_or_default();
    let brand_trim = brand.trim();
    if !brand_trim.is_empty() && brand_trim != "Unknown" {
        return brand_trim.to_string();
    }

    // OS-specific fallbacks
    #[cfg(target_os = "linux")]
    {
        // Parse /proc/cpuinfo for a reasonable model name
        if let Ok(cpuinfo) = std::fs::read_to_string("/proc/cpuinfo") {
            let mut model: Option<String> = None;
            for line in cpuinfo.lines() {
                if let Some((k, v)) = line.split_once(':') {
                    let key = k.trim().to_ascii_lowercase();
                    let val = v.trim();
                    if key == "model name" && !val.is_empty() {
                        model = Some(val.to_string());
                        break;
                    }
                    // ARM variants often use other keys
                    if (key == "processor" || key == "hardware" || key == "model") && !val.is_empty() {
                        model.get_or_insert(val.to_string());
                    }
                }
            }
            if let Some(m) = model { return m; }
        }
    }

    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        if let Ok(out) = Command::new("/usr/sbin/sysctl").args(["-n", "machdep.cpu.brand_string"]).output() {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !s.is_empty() { return s; }
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(s) = std::env::var("PROCESSOR_IDENTIFIER") {
            let s = s.trim().to_string();
            if !s.is_empty() { return s; }
        }
    }

    // Fallback to vendor + brand combo if vendor can help
    let vendor = sys.cpus().first().map(|c| c.vendor_id().to_string()).unwrap_or_default();
    if !vendor.is_empty() && vendor != "Unknown" {
        if brand_trim.is_empty() || brand_trim == "Unknown" {
            return vendor.to_string();
        }
        return format!("{} {}", vendor, brand_trim);
    }

    String::from("Unknown")
}

// -------- Disks helpers --------
#[derive(Debug, Clone)]
struct DiskInfo {
    dev: String,
    mount: String,
    fs: String,
    total: u64,
    used: u64,
    pct: f32,
    temp_c: Option<f32>,
}

fn fmt_bytes_gib(bytes: u64) -> String {
    let gib = bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    if gib >= 100.0 {
        format!("{:.0} GiB", gib)
    } else if gib >= 10.0 {
        format!("{:.1} GiB", gib)
    } else {
        format!("{:.2} GiB", gib)
    }
}

fn fmt_bytes(bytes: u64) -> String {
    // Human readable bytes: uses binary units
    let mut v = bytes as f64;
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut i = 0;
    while v >= 1024.0 && i + 1 < units.len() {
        v /= 1024.0;
        i += 1;
    }
    if v >= 100.0 { format!("{:.0} {}", v, units[i]) }
    else if v >= 10.0 { format!("{:.1} {}", v, units[i]) }
    else { format!("{:.2} {}", v, units[i]) }
}

#[cfg(target_os = "linux")]
fn list_disks_best_effort() -> Vec<DiskInfo> {
    let mounts = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
    let mut out: Vec<DiskInfo> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for line in mounts.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 3 { continue; }
        let dev = parts[0];
        let mnt = parts[1];
        let fs = parts[2];
        if !dev.starts_with("/dev/") { continue; }
        let key = format!("{}@{}", dev, mnt);
        if !seen.insert(key) { continue; }
        if let Some((total, avail)) = stat_mount_space(mnt) {
            let used = total.saturating_sub(avail);
            let pct = if total > 0 { (used as f32 / total as f32) * 100.0 } else { 0.0 };
            let temp_c = read_disk_temperature_c(dev);
            out.push(DiskInfo { dev: dev.to_string(), mount: mnt.to_string(), fs: fs.to_string(), total, used, pct, temp_c });
        }
    }
    out
}

#[cfg(not(target_os = "linux"))]
fn list_disks_best_effort() -> Vec<DiskInfo> { Vec::new() }

#[cfg(target_os = "linux")]
#[allow(non_camel_case_types)]
#[repr(C)]
struct statvfs_t {
    f_bsize: u64,
    f_frsize: u64,
    f_blocks: u64,
    f_bfree: u64,
    f_bavail: u64,
    f_files: u64,
    f_ffree: u64,
    f_favail: u64,
    f_fsid: u64,
    f_flag: u64,
    f_namemax: u64,
}

#[cfg(target_os = "linux")]
unsafe extern "C" { fn statvfs(path: *const i8, buf: *mut statvfs_t) -> i32; }

#[cfg(target_os = "linux")]
fn stat_mount_space(path: &str) -> Option<(u64, u64)> {
    use std::ffi::CString;
    let cpath = CString::new(path).ok()?;
    let mut st = statvfs_t { f_bsize: 0, f_frsize: 0, f_blocks: 0, f_bfree: 0, f_bavail: 0, f_files: 0, f_ffree: 0, f_favail: 0, f_fsid: 0, f_flag: 0, f_namemax: 0 };
    let rc = unsafe { statvfs(cpath.as_ptr(), &mut st as *mut statvfs_t) };
    if rc != 0 { return None; }
    let fr = if st.f_frsize > 0 { st.f_frsize } else { st.f_bsize };
    let total = st.f_blocks.saturating_mul(fr);
    let avail = st.f_bavail.saturating_mul(fr);
    Some((total, avail))
}

#[cfg(not(target_os = "linux"))]
fn stat_mount_space(_path: &str) -> Option<(u64, u64)> { None }

#[cfg(target_os = "linux")]
fn read_disk_temperature_c(devnode: &str) -> Option<f32> {
    use std::path::Path;
    // Strip /dev/ prefix and partition suffix
    let name = devnode.strip_prefix("/dev/").unwrap_or(devnode);
    let base = if name.starts_with("nvme") {
        // nvme0n1p2 -> nvme0n1
        match name.rsplit_once('p') {
            Some((left, _)) => left.to_string(),
            None => name.to_string(),
        }
    } else {
        // sda1 -> sda; mmcblk0p1 -> mmcblk0
        let mut b = name.to_string();
        if name.starts_with("mmcblk") {
            if let Some(pos) = name.find('p') { b = name[..pos].to_string(); }
        } else {
            while b.chars().last().map_or(false, |c| c.is_ascii_digit()) { b.pop(); }
        }
        b
    };
    let hwmon_root = Path::new("/sys/block").join(&base).join("device").join("hwmon");
    let entries = std::fs::read_dir(&hwmon_root).ok()?;
    let mut best: Option<f32> = None;
    for ent in entries.flatten() {
        let hpath = ent.path();
        if !hpath.is_dir() { continue; }
        if let Ok(files) = std::fs::read_dir(&hpath) {
            for f in files.flatten() {
                let p = f.path();
                if let Some(fname) = p.file_name().and_then(|s| s.to_str()) {
                    if fname.starts_with("temp") && fname.ends_with("_input") {
                        if let Ok(raw) = std::fs::read_to_string(&p) {
                            if let Ok(mut v) = raw.trim().parse::<f32>() {
                                if v > 200.0 { v = v / 1000.0; }
                                best = Some(best.map_or(v, |b| b.max(v)));
                            }
                        }
                    }
                }
            }
        }
    }
    best
}

#[cfg(not(target_os = "linux"))]
fn read_disk_temperature_c(_devnode: &str) -> Option<f32> { None }

// -------- Network counters (Linux best effort) --------
#[cfg(target_os = "linux")]
fn read_network_counters() -> Vec<(String, u64, u64)> {
    let mut out: Vec<(String, u64, u64)> = Vec::new();
    let dir = match std::fs::read_dir("/sys/class/net") { Ok(d) => d, Err(_) => return out };
    for ent in dir.flatten() {
        let name = match ent.file_name().into_string() { Ok(s) => s, Err(_) => continue };
        // Skip entries that are not real directories
        let path = ent.path();
        if !path.is_dir() { continue; }
        let rx_p = path.join("statistics").join("rx_bytes");
        let tx_p = path.join("statistics").join("tx_bytes");
        let rx = std::fs::read_to_string(&rx_p).ok().and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0);
        let tx = std::fs::read_to_string(&tx_p).ok().and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0);
        out.push((name, rx, tx));
    }
    out
}

#[cfg(not(target_os = "linux"))]
fn read_network_counters() -> Vec<(String, u64, u64)> { Vec::new() }

// All services listing (Linux)
#[cfg(target_os = "linux")]
fn get_all_services() -> Vec<(String, String, String)> {
    // Use systemctl to list all services without pager/legend
    let output = Command::new("systemctl")
        .args(["list-units", "--type=service", "--all", "--no-legend", "--no-pager"])
        .output();
    let mut services: Vec<(String, String, String)> = Vec::new();
    if let Ok(out) = output {
        if out.status.success() {
            if let Ok(text) = String::from_utf8(out.stdout) {
                for line in text.lines() {
                    let l = line.trim();
                    if l.is_empty() { continue; }
                    // Expected columns: UNIT LOAD ACTIVE SUB DESCRIPTION
                    let toks: Vec<&str> = l.split_whitespace().collect();
                    if toks.len() >= 5 {
                        let unit = toks[0].to_string();
                        let active = toks[2].to_string();
                        let desc = toks[4..].join(" ");
                        services.push((unit, active, desc));
                    }
                }
            }
        }
    }
    // Sort by unit name for stable display
    services.sort_by(|a, b| a.0.cmp(&b.0));
    services
}

#[cfg(not(target_os = "linux"))]
fn get_all_services() -> Vec<(String, String, String)> { Vec::new() }

// Fetch detailed status text for a service (Linux)
#[cfg(target_os = "linux")]
fn get_service_status(unit: &str) -> String {
    let output = Command::new("systemctl")
        .args(["status", unit, "--no-pager", "--full"]) 
        .output();
    match output {
        Ok(out) if out.status.success() => String::from_utf8(out.stdout).unwrap_or_else(|_| String::from("(failed to decode output)")),
        Ok(out) => {
            let mut s = String::new();
            if !out.stdout.is_empty() {
                s = String::from_utf8_lossy(&out.stdout).into_owned();
            }
            if !out.stderr.is_empty() {
                if !s.is_empty() { s.push_str("\n"); }
                s.push_str(&String::from_utf8_lossy(&out.stderr));
            }
            if s.is_empty() { s = String::from("(no output)"); }
            s
        }
        Err(_) => String::from("systemctl not available or failed to execute."),
    }
}

#[cfg(not(target_os = "linux"))]
fn get_service_status(_unit: &str) -> String { String::from("Service details are supported on Linux only.") }

// -------- Process details helpers (Linux best-effort) --------
#[cfg(target_os = "linux")]
fn get_process_name(pid: i32) -> String {
    let comm_path = format!("/proc/{}/comm", pid);
    if let Ok(s) = std::fs::read_to_string(&comm_path) {
        let name = s.trim();
        if !name.is_empty() { return name.to_string(); }
    }
    let status_path = format!("/proc/{}/status", pid);
    if let Ok(s) = std::fs::read_to_string(&status_path) {
        if let Some(line) = s.lines().find(|l| l.starts_with("Name:")) {
            return line[5..].trim().to_string();
        }
    }
    String::new()
}

#[cfg(not(target_os = "linux"))]
fn get_process_name(_pid: i32) -> String { String::new() }

#[cfg(target_os = "linux")]
fn get_process_details(pid: i32) -> String {
    use std::path::PathBuf;
    let mut out = String::new();
    let status_path = format!("/proc/{}/status", pid);
    let stat_path = format!("/proc/{}/stat", pid);
    let cmdline_path = format!("/proc/{}/cmdline", pid);
    let exe_path = PathBuf::from(format!("/proc/{}/exe", pid));
    let cwd_path = PathBuf::from(format!("/proc/{}/cwd", pid));

    // Name, State, PPID, Uid, Gid, Threads, VmSize, VmRSS
    if let Ok(s) = std::fs::read_to_string(&status_path) {
        for key in ["Name:", "State:", "PPid:", "Uid:", "Gid:", "Threads:", "VmSize:", "VmRSS:"].iter() {
            if let Some(line) = s.lines().find(|l| l.starts_with(key)) {
                out.push_str(line.trim());
                out.push('\n');
            }
        }
    } else {
        out.push_str("(status not available)\n");
    }

    // Priority/Nice
    if let Ok(s) = std::fs::read_to_string(&stat_path) {
        if let Some(rparen) = s.rfind(')') {
            let after = &s[rparen+2..];
            let toks: Vec<&str> = after.split_whitespace().collect();
            let pri = toks.get(15).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
            let ni = toks.get(16).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
            out.push_str(&format!("Priority: {}\nNice: {}\n", pri, ni));
        }
    }

    // Exe and CWD
    if let Ok(p) = std::fs::read_link(&exe_path) { out.push_str(&format!("Exe: {}\n", p.to_string_lossy())); }
    if let Ok(p) = std::fs::read_link(&cwd_path) { out.push_str(&format!("Cwd: {}\n", p.to_string_lossy())); }

    // Cmdline
    if let Ok(raw) = std::fs::read(&cmdline_path) {
        if !raw.is_empty() {
            let parts: Vec<String> = raw.split(|b| *b == 0).filter(|s| !s.is_empty()).map(|s| String::from_utf8_lossy(s).into_owned()).collect();
            if !parts.is_empty() {
                out.push_str("Cmdline: ");
                out.push_str(&parts.join(" "));
                out.push('\n');
            }
        }
    }

    // Open file descriptors count
    let fd_dir = format!("/proc/{}/fd", pid);
    if let Ok(rd) = std::fs::read_dir(&fd_dir) {
        let count = rd.filter(|e| e.is_ok()).count();
        out.push_str(&format!("FDs: {}\n", count));
    }

    if out.is_empty() { out = String::from("(no details)"); }
    out
}

#[cfg(not(target_os = "linux"))]
fn get_process_details(pid: i32) -> String { format!("Process details are supported on Linux only. PID {}", pid) }

// -------- Logs helpers --------
#[derive(Clone)]
struct LogEntry { name: String, path: String, size: u64, modified: String }

fn list_var_log_files() -> Vec<LogEntry> {
    use std::fs;
    use std::path::{Path, PathBuf};
    let root = Path::new("/var/log");
    let mut out: Vec<LogEntry> = Vec::new();
    let mut stack: Vec<PathBuf> = Vec::new();
    // Seed with immediate children of /var/log
    if let Ok(rd) = fs::read_dir(root) {
        for ent in rd.flatten() {
            stack.push(ent.path());
        }
    } else {
        return out;
    }

    while let Some(p) = stack.pop() {
        // Skip systemd journal binary logs entirely
        if p.starts_with(root.join("journal")) { continue; }
        // Use symlink_metadata to decide what to do without following dir symlinks
        let md = match fs::symlink_metadata(&p) { Ok(m) => m, Err(_) => continue };
        if md.is_file() {
            // Build display name as relative path under /var/log when possible
            let name = match p.strip_prefix(root) {
                Ok(rel) => rel.to_string_lossy().to_string(),
                Err(_) => p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| p.to_string_lossy().to_string()),
            };
            let size = md.len();
            let mstr = md.modified().ok().map(fmt_system_time).unwrap_or_else(|| String::from("-"));
            out.push(LogEntry { name, path: p.to_string_lossy().to_string(), size, modified: mstr });
        } else if md.is_dir() {
            // Do not follow directory symlinks to avoid cycles
            if md.file_type().is_symlink() { continue; }
            if let Ok(rd) = fs::read_dir(&p) {
                for ent in rd.flatten() {
                    stack.push(ent.path());
                }
            }
        } else if md.file_type().is_symlink() {
            // If it's a symlink, try following only if it points to a file
            if let Ok(target_md) = fs::metadata(&p) {
                if target_md.is_file() {
                    let name = match p.strip_prefix(root) {
                        Ok(rel) => rel.to_string_lossy().to_string(),
                        Err(_) => p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| p.to_string_lossy().to_string()),
                    };
                    let size = target_md.len();
                    let mstr = target_md.modified().ok().map(fmt_system_time).unwrap_or_else(|| String::from("-"));
                    out.push(LogEntry { name, path: p.to_string_lossy().to_string(), size, modified: mstr });
                }
            }
        }
    }

    // Sort by name (relative path) for stable listing
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn fmt_system_time(st: std::time::SystemTime) -> String {
    use std::time::UNIX_EPOCH;
    match st.duration_since(UNIX_EPOCH) {
        Ok(dur) => {
            let secs = dur.as_secs();
            // Simple YYYY-MM-DD HH:MM formatting using chrono-like manual approach
            // To avoid extra deps, just show seconds since epoch
            format!("{}s", secs)
        }
        Err(_) => String::from("-"),
    }
}

// Build a simple "user@hostname" system prompt string (best effort, no extra deps).
fn get_hostname_best_effort() -> String {
    // Try Linux-specific files first
    for path in ["/proc/sys/kernel/hostname", "/etc/hostname"] {
        if let Ok(s) = std::fs::read_to_string(path) {
            let t = s.trim();
            if !t.is_empty() { return t.to_string(); }
        }
    }
    if let Ok(h) = std::env::var("HOSTNAME") { if !h.trim().is_empty() { return h; } }
    String::from("host")
}

fn build_system_prompt() -> String {
    let user = std::env::var("USER").unwrap_or_else(|_| String::from("user"));
    let host = get_hostname_best_effort();
    format!("{}@{}", user, host)
}

fn read_log_file_best_effort(path: &str, sudo_pass: Option<&str>) -> Result<String, String> {
    // First try normal read
    match std::fs::read_to_string(path) {
        Ok(mut s) => {
            cap_log_text(&mut s);
            return Ok(s);
        }
        Err(e) => {
            // If permission denied and sudo password provided, try sudo -S cat
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                if let Some(pw) = sudo_pass {
                    let mut child = match Command::new("sudo")
                        .arg("-S")
                        .arg("--")
                        .arg("cat")
                        .arg(path)
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .spawn() {
                        Ok(c) => c,
                        Err(spawn_err) => return Err(format!("Failed to spawn sudo: {}", spawn_err)),
                    };
                    if let Some(mut stdin) = child.stdin.take() {
                        use std::io::Write;
                        let _ = stdin.write_all(pw.as_bytes());
                        let _ = stdin.write_all(b"\n");
                    }
                    let out = child.wait_with_output().map_err(|e| format!("sudo error: {}", e))?;
                    if out.status.success() {
                        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
                        cap_log_text(&mut text);
                        return Ok(text);
                    } else {
                        let err = String::from_utf8_lossy(&out.stderr).into_owned();
                        return Err(if err.trim().is_empty() { String::from("sudo failed") } else { err });
                    }
                }
            }
            // Other errors or no password
            return Err(format!("{}", e));
        }
    }
}

fn cap_log_text(s: &mut String) {
    // Keep at most last ~5000 lines to avoid huge popups
    let max_lines = 5000usize;
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() > max_lines {
        let tail = &lines[lines.len()-max_lines..];
        *s = tail.join("\n");
    }
}


// -------- Applications detection helpers --------
fn is_cmd_in_path(bin: &str) -> bool {
    if let Ok(path) = std::env::var("PATH") {
        for p in path.split(':') {
            let mut cand = std::path::PathBuf::from(p);
            cand.push(bin);
            if cand.is_file() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(md) = std::fs::metadata(&cand) {
                        let mode = md.permissions().mode();
                        if mode & 0o111 != 0 { return true; }
                    }
                }
                #[cfg(not(unix))]
                {
                    // On non-unix, assume presence means usable
                    return true;
                }
            }
        }
    }
    false
}

fn any_cmd_in_path(cands: &[&str]) -> bool {
    cands.iter().any(|c| is_cmd_in_path(c))
}

#[cfg(target_os = "linux")]
fn is_any_unit_active(units: &[&str]) -> Option<bool> {
    for u in units {
        let unit = if u.ends_with(".service") { (*u).to_string() } else { format!("{}.service", u) };
        if let Ok(out) = Command::new("systemctl").args(["is-active", &unit]).output() {
            if out.status.success() {
                if let Ok(s) = String::from_utf8(out.stdout) {
                    if s.trim() == "active" { return Some(true); }
                }
            }
        }
    }
    Some(false)
}

#[cfg(not(target_os = "linux"))]
fn is_any_unit_active(_units: &[&str]) -> Option<bool> { None }

fn build_applications_status() -> Vec<(String, String, String)> {
    // Returns (Application, Active, Installed)
    let mut rows: Vec<(String, String, String)> = Vec::new();

    // Helper to map Option<bool> to string
    let yn = |b: Option<bool>| -> String {
        match b {
            Some(true) => "Yes".to_string(),
            Some(false) => "No".to_string(),
            None => "N/A".to_string(),
        }
    };

    // Apache2
    let apache_inst = any_cmd_in_path(&["apache2", "httpd"]);
    let apache_active = is_any_unit_active(&["apache2", "httpd"]);
    rows.push(("Apache2".to_string(), yn(apache_active), if apache_inst { "Yes" } else { "No" }.to_string()));

    // Nginx
    let nginx_inst = any_cmd_in_path(&["nginx"]);
    let nginx_active = is_any_unit_active(&["nginx"]);
    rows.push(("Nginx".to_string(), yn(nginx_active), if nginx_inst { "Yes" } else { "No" }.to_string()));

    // Postgresql
    let pg_inst = any_cmd_in_path(&["postgres", "psql"]);
    let pg_active = is_any_unit_active(&["postgresql", "postgresql@"]);
    rows.push(("Postgresql".to_string(), yn(pg_active), if pg_inst { "Yes" } else { "No" }.to_string()));

    // Mysql (include MariaDB)
    let my_inst = any_cmd_in_path(&["mysqld", "mariadbd", "mysql"]);
    let my_active = is_any_unit_active(&["mysql", "mariadb"]);
    rows.push(("Mysql".to_string(), yn(my_active), if my_inst { "Yes" } else { "No" }.to_string()));

    // Podman
    let pod_inst = any_cmd_in_path(&["podman"]);
    let pod_active = is_any_unit_active(&["podman"]);
    rows.push(("Podman".to_string(), yn(pod_active), if pod_inst { "Yes" } else { "No" }.to_string()));

    // Docker
    let dock_inst = any_cmd_in_path(&["docker"]);
    let dock_active = is_any_unit_active(&["docker"]);
    rows.push(("Docker".to_string(), yn(dock_active), if dock_inst { "Yes" } else { "No" }.to_string()));

    rows
}

// Hardware manufacturer and model detection (best-effort)
#[cfg(target_os = "linux")]
fn get_hw_manufacturer_and_model() -> (Option<String>, Option<String>) {
    let read = |p: &str| -> Option<String> {
        std::fs::read_to_string(p).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    };
    let manufacturer = read("/sys/class/dmi/id/sys_vendor")
        .or_else(|| read("/sys/devices/virtual/dmi/id/sys_vendor"));
    let model = read("/sys/class/dmi/id/product_name")
        .or_else(|| read("/sys/devices/virtual/dmi/id/product_name"))
        .or_else(|| read("/proc/device-tree/model"));
    (manufacturer, model)
}

#[cfg(not(target_os = "linux"))]
fn get_hw_manufacturer_and_model() -> (Option<String>, Option<String>) { (None, None) }

/// Draw the main content: top tabs (CPU/Graphics/Memory) and the Processes area.
fn draw_header(
    f: &mut ratatui::Frame<'_>,
    area: Rect,
    sys: &System,
    app: &App,
) {
    // Best-effort CPU temperature (Linux): read from hwmon/thermal sysfs when available
    let cpu_temp_c = read_cpu_temperature_c();
    let global_cpu = sys.global_cpu_usage(); // percent
    let used_mem = sys.used_memory(); // KiB
    let total_mem = sys.total_memory(); // KiB
    let _mem_pct = if total_mem > 0 { (used_mem as f32 / total_mem as f32) * 100.0 } else { 0.0 };

    // Split area vertically into: Top Tabs bar + content, Memory, Processes
    // CPU tab content height = number of core rows (max 8) + 2 (2 lines of text)
    let cpu_rows_for_height = sys.cpus().len().min(8) as u16;
    // CPU tab height: rows for gauges + RAM/SWAP row + 3 separator lines (top/below-mem/bottom)
    let cpu_height = cpu_rows_for_height + 4;

    // Use cached GPUs for Graphics tab content sizing
    let gpus = &app.gpus;
    let gfx_height: u16 = (gpus.len() as u16 + 1 + 2).max(3); // header + rows + block borders
    // Applications frame target height (header + rows + borders). Ensure it can fit all apps.
    let apps_rows: u16 = build_applications_status().len() as u16; // typically 6 rows
    let apps_block_height: u16 = (apps_rows + 1 + 2).max(3);
    // Height for the combined Applications | GPU row should fit the larger of the two
    let gfx_app_height: u16 = gfx_height.max(apps_block_height);
    let sys_block_height: u16 = 6 + 2; // 6 info lines (Manufacturer with Hardware Model inline, Processor Model, OS Name, Kernel, OS version, Hostname) + block borders
    let cpu_block_height: u16 = 6 + 2; // 6 info lines + block borders (Cores, Threads, CPU%, Load, Uptime, Temp)
    let mem_block_height: u16 = 6 + 2; // 4 info lines + 2 gauge rows (RAM, SWAP) + block borders
    let top_frames_height: u16 = sys_block_height.max(cpu_block_height).max(mem_block_height);

    // Disks info for System tab Disks frame sizing (best-effort, Linux-focused)
    let disks = list_disks_best_effort();
    let disks_block_height: u16 = (disks.len() as u16 + 1 + 2).max(3); // header + rows + borders
    let proc_block_height: u16 = 12; // Process frame fixed height (inner ~10 rows) + borders

    // Decide current top content height based on selected tab (0 = Dashboard, 1 = top/htop, 2 = Services, 4 = Logs)
    let top_content_height = match app.selected_top_tab {
        0 => top_frames_height + gfx_height + disks_block_height + proc_block_height,
        1 => cpu_height,
        2 => cpu_height.max(5), // Services tab height (min)
        3 => cpu_height.max(5), // Shell tab height (min)
        4 => cpu_height.max(5), // Logs tab height (min)
        _ => cpu_height,
    };

    // Layout: remove visual top Tabs bar and use full area for content for known tabs
    let constraints = if matches!(app.selected_top_tab, 0 | 1 | 2 | 3 | 4) {
        [
            Constraint::Min(5),    // content fills remaining space
        ]
    } else {
        [
            Constraint::Length(top_content_height), // tab content
        ]
    };
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    // Top tab content area (no Tabs bar)
    let top_area = main_chunks[0];

    if app.selected_top_tab == 1 {
        // CPU content in tab 1 (frameless)
        let cpu_inner = top_area;
        // Determine how many rows the per-core gauge grid will occupy (max 8)
        let gauges_rows_for_layout: u16 = sys.cpus().len().min(8) as u16;

        // Layout: top separator, RAM/SWAP row, separator below RAM/SWAP, gauges, separator below gauges, process list area
        let cpu_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // top separator
                Constraint::Length(1), // RAM/SWAP gauges row
                Constraint::Length(1), // separator below RAM/SWAP
                Constraint::Length(gauges_rows_for_layout),    // per-core charts (fixed rows)
                Constraint::Length(1), // separator below gauges
                Constraint::Min(1), // process table area at bottom (expand to fill)
            ])
            .split(cpu_inner);

        // Draw top separator line
        {
            let sep_w = cpu_chunks[0].width as usize;
            if sep_w > 0 {
                let sep = "─".repeat(sep_w);
                let sep_par = ratatui::widgets::Paragraph::new(Line::from(Span::styled(
                    sep,
                    Style::default().fg(Color::DarkGray),
                )));
                f.render_widget(sep_par, cpu_chunks[0]);
            }
        }

        // RAM and SWAP gauges row (2 columns)
        {
            let memswap_cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Ratio(1, 2),
                    Constraint::Length(1), // vertical separator column
                    Constraint::Ratio(1, 2),
                ])
                .split(cpu_chunks[1]);

            // Memory gauge
            let mem_ratio = if total_mem > 0 { (used_mem as f64 / total_mem as f64).clamp(0.0, 1.0) } else { 0.0 };
            let mem_label = format!("RAM {:>5.1}%", if total_mem > 0 { (used_mem as f32 / total_mem as f32) * 100.0 } else { 0.0 });
            let mem_gauge = Gauge::default()
                .gauge_style(Style::default().fg(Color::Yellow))
                .label(Span::raw(mem_label))
                .ratio(mem_ratio)
                .use_unicode(true);
            f.render_widget(mem_gauge, memswap_cols[0]);

            // Vertical separator between RAM and SWAP
            let vline = ratatui::widgets::Paragraph::new(Line::from(Span::styled(
                "│",
                Style::default().fg(Color::DarkGray),
            )));
            f.render_widget(vline, memswap_cols[1]);

            // Swap gauge
            let used_swap = sys.used_swap();
            let total_swap = sys.total_swap();
            let swap_ratio = if total_swap > 0 { (used_swap as f64 / total_swap as f64).clamp(0.0, 1.0) } else { 0.0 };
            let swap_pct = if total_swap > 0 { (used_swap as f32 / total_swap as f32) * 100.0 } else { 0.0 };
            let swap_label = format!("SWAP {:>5.1}%", swap_pct);
            let swap_gauge = Gauge::default()
                .gauge_style(Style::default().fg(Color::Magenta))
                .label(Span::raw(swap_label))
                .ratio(swap_ratio)
                .use_unicode(true);
            f.render_widget(swap_gauge, memswap_cols[2]);
        }

        // Draw the separator line below RAM/SWAP row
        {
            let sep_w = cpu_chunks[2].width as usize;
            if sep_w > 0 {
                let sep = "─".repeat(sep_w);
                let sep_par = ratatui::widgets::Paragraph::new(Line::from(Span::styled(
                    sep,
                    Style::default().fg(Color::DarkGray),
                )));
            f.render_widget(sep_par, cpu_chunks[2]);
            }
        }

        // Build per-core CPU usage values and labels
        let cpus = sys.cpus();
        let core_count = cpus.len();
        let mut labels_owned: Vec<String> = Vec::with_capacity(core_count);
        let mut values: Vec<u64> = Vec::with_capacity(core_count);
        for (i, c) in cpus.iter().enumerate() {
            labels_owned.push(format!("C{}", i));
            values.push(c.cpu_usage().clamp(0.0, 100.0).round() as u64);
        }

        // Arrange per-core mini charts in up to 8 rows, adding columns as needed
        if core_count > 0 {
            let rows = core_count.min(8);
            let cols = (core_count + rows - 1) / rows;

            // Split the available area into rows
            let row_constraints: Vec<Constraint> = (0..rows)
                .map(|_| Constraint::Ratio(1, rows as u32))
                .collect();
            let row_areas = Layout::default()
                .direction(Direction::Vertical)
                .constraints(row_constraints)
                .split(cpu_chunks[3]);

            for r in 0..rows {
                // Split each row into columns
                let col_constraints: Vec<Constraint> = (0..cols)
                    .map(|_| Constraint::Ratio(1, cols as u32))
                    .collect();
                let col_areas_base = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints(col_constraints)
                    .split(row_areas[r]);

                // If there are exactly two columns, insert a vertical separator between them
                let (col_areas, has_middle_sep) = if cols == 2 {
                    let areas = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([
                            Constraint::Ratio(1, 2),
                            Constraint::Length(1), // vertical separator column
                            Constraint::Ratio(1, 2),
                        ])
                        .split(row_areas[r]);
                    (areas, true)
                } else {
                    (col_areas_base, false)
                };

                // Render middle vertical separator when applicable (once per row)
                if has_middle_sep {
                    let vline = ratatui::widgets::Paragraph::new(Line::from(Span::styled(
                        "│",
                        Style::default().fg(Color::DarkGray),
                    )));
                    f.render_widget(vline, col_areas[1]);
                }

                for c in 0..cols {
                    let idx = r + c * rows; // column-major to keep rows <= 8
                    if idx >= core_count {
                        continue;
                    }
                    let ratio = (values[idx] as f64 / 100.0).clamp(0.0, 1.0);
                    // Determine the drawing rect for this column (accounting for optional separator)
                    let cell_area = if has_middle_sep {
                        if c == 0 { col_areas[0] } else { col_areas[2] }
                    } else {
                        col_areas[c]
                    };
                    // Split cell into left label and right gauge
                    let label_text = format!("CPU {}", labels_owned[idx]);
                    let parts = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([
                            Constraint::Length(8), // left fixed width for label (e.g., "CPU C12")
                            Constraint::Min(1),    // right gauge fills the rest
                        ])
                        .split(cell_area);

                    // Render right-aligned label on the left side
                    let text_par = ratatui::widgets::Paragraph::new(Line::from(Span::raw(label_text)))
                        .alignment(ratatui::layout::Alignment::Right);
                    f.render_widget(text_par, parts[0]);

                    // Render gauge without label on the right, with dynamic color
                    let cpu_pct = values[idx] as u64;
                    let color = if cpu_pct > 90 {
                        Color::Red
                    } else if cpu_pct >= 70 {
                        Color::Rgb(255, 191, 0) // amber
                    } else {
                        Color::Green
                    };
                    let mini = Gauge::default()
                        .gauge_style(Style::default().fg(color))
                        .ratio(ratio)
                        .use_unicode(true);
                    f.render_widget(mini, parts[1]);
                }
            }
        }
        // Draw separator line below gauges (before process list)
        {
            let sep_w = cpu_chunks[4].width as usize;
            if sep_w > 0 {
                let sep = "─".repeat(sep_w);
                let sep_par = ratatui::widgets::Paragraph::new(Line::from(Span::styled(
                    sep,
                    Style::default().fg(Color::DarkGray),
                )));
                f.render_widget(sep_par, cpu_chunks[4]);
            }
        }

        // Render Processes as a scrollable selectable Table at the bottom of the top/htop tab
        {
            use std::collections::HashMap;
            use std::fs;

            let proc_area = cpu_chunks[5];
            // Rows per page (minus header)
            let rows_per_page = proc_area.height.saturating_sub(1) as usize;

            // Clamp selection and compute window start based on selection
            let total = app.procs_pids_sorted.len();
            let selected = app.procs_selected.min(total.saturating_sub(1));
            let max_start = total.saturating_sub(rows_per_page);
            let mut start = app.procs_scroll.min(max_start);
            if selected < start { start = selected; }
            if rows_per_page > 0 && selected >= start + rows_per_page { start = selected + 1 - rows_per_page; }

            // Build a uid->username map from /etc/passwd (simple parser)
            let mut uid_name: HashMap<u32, String> = HashMap::new();
            if let Ok(passwd) = fs::read_to_string("/etc/passwd") {
                for line in passwd.lines() {
                    if line.trim().is_empty() || line.starts_with('#') { continue; }
                    let parts: Vec<&str> = line.split(':').collect();
                    if parts.len() > 3 {
                        if let Ok(uid) = parts[2].parse::<u32>() {
                            uid_name.insert(uid, parts[0].to_string());
                        }
                    }
                }
            }

            // Header
            let header = Row::new(vec![
                Cell::from(Span::styled("PID", Style::default().add_modifier(Modifier::BOLD))),
                Cell::from(Span::styled("USER", Style::default().add_modifier(Modifier::BOLD))),
                Cell::from(Span::styled("PRI", Style::default().add_modifier(Modifier::BOLD))),
                Cell::from(Span::styled("NI", Style::default().add_modifier(Modifier::BOLD))),
                Cell::from(Span::styled("CPU%", Style::default().add_modifier(Modifier::BOLD))),
                Cell::from(Span::styled("MEM%", Style::default().add_modifier(Modifier::BOLD))),
                Cell::from(Span::styled("TIME", Style::default().add_modifier(Modifier::BOLD))),
                Cell::from(Span::styled("CMD", Style::default().add_modifier(Modifier::BOLD))),
            ]);

            // Build rows from cached PID ordering
            let mut rows: Vec<Row> = Vec::new();
            let total_mem_kib_f = total_mem as f32;
            for (i, pid) in app.procs_pids_sorted.iter().cloned().skip(start).take(rows_per_page).enumerate() {
                // Find process by pid from sys
                let mut cpu = 0.0f32;
                let mut mem_pct = 0.0f32;
                let mut time_str = String::new();
                let mut cmd = String::new();
                if let Some((_, p)) = sys.processes().iter().find(|(p, _)| p.as_u32() as i32 == pid) {
                    cpu = p.cpu_usage();
                    let mem_kib = p.memory();
                    mem_pct = if total_mem_kib_f > 0.0 { (mem_kib as f32 / total_mem_kib_f) * 100.0 } else { 0.0 };
                    let secs = p.run_time();
                    let days = secs / 86_400;
                    let hours = (secs % 86_400) / 3_600;
                    let minutes = (secs % 3_600) / 60;
                    let seconds = secs % 60;
                    time_str = if days > 0 { format!("{}d {:02}:{:02}:{:02}", days, hours, minutes, seconds) } else { format!("{:02}:{:02}:{:02}", hours, minutes, seconds) };
                    cmd = if !p.cmd().is_empty() { p.cmd().join(std::ffi::OsStr::new(" ")).to_string_lossy().into_owned() } else { p.name().to_string_lossy().into_owned() };
                }
                // USER via /proc
                let user = {
                    let status_path = format!("/proc/{}/status", pid);
                    if let Ok(s) = fs::read_to_string(&status_path) {
                        if let Some(uid_line) = s.lines().find(|l| l.starts_with("Uid:")) {
                            let mut it = uid_line.split_whitespace();
                            it.next();
                            if let Some(uid_str) = it.next() {
                                if let Ok(uid) = uid_str.parse::<u32>() {
                                    uid_name.get(&uid).cloned().unwrap_or_else(|| uid.to_string())
                                } else { String::from("?") }
                            } else { String::from("?") }
                        } else { String::from("?") }
                    } else { String::from("?") }
                };
                // PRI/NI via /proc
                let (pri, ni) = {
                    let stat_path = format!("/proc/{}/stat", pid);
                    if let Ok(s) = fs::read_to_string(&stat_path) {
                        if let Some(rparen) = s.rfind(')') {
                            let after = &s[rparen+2..];
                            let toks: Vec<&str> = after.split_whitespace().collect();
                            let pri = toks.get(15).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
                            let ni = toks.get(16).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
                            (pri, ni)
                        } else { (0, 0) }
                    } else { (0, 0) }
                };
                // Build row
                let mut row = Row::new(vec![
                    Cell::from(Span::raw(format!("{:>6}", pid))),
                    Cell::from(Span::raw(format!("{:<8}", user))),
                    Cell::from(Span::raw(format!("{:>3}", pri))),
                    Cell::from(Span::raw(format!("{:>3}", ni))),
                    Cell::from(Span::raw(format!("{:>6.1}", cpu))),
                    Cell::from(Span::raw(format!("{:>7.1}", mem_pct))),
                    Cell::from(Span::raw(format!("{:>10}", time_str))),
                    Cell::from(Span::raw(cmd)),
                ]);
                if start + i == selected {
                    row = row.style(Style::default().add_modifier(Modifier::REVERSED));
                }
                rows.push(row);
            }
            if rows.is_empty() {
                rows.push(Row::new(vec![
                    Cell::from(Span::raw("No processes.")),
                    Cell::from(Span::raw("")),
                    Cell::from(Span::raw("")),
                    Cell::from(Span::raw("")),
                    Cell::from(Span::raw("")),
                    Cell::from(Span::raw("")),
                    Cell::from(Span::raw("")),
                    Cell::from(Span::raw("")),
                ]));
            }

            // Column widths: PID 6, USER 8, PRI 3, NI 3, CPU% 6, MEM% 7, TIME 10, CMD fills rest
            let table = Table::new(
                rows,
                [
                    Constraint::Length(6),
                    Constraint::Length(8),
                    Constraint::Length(3),
                    Constraint::Length(3),
                    Constraint::Length(6),
                    Constraint::Length(7),
                    Constraint::Length(10),
                    Constraint::Min(10),
                ],
            )
            .header(header)
            .block(Block::default());

            f.render_widget(table, proc_area);
        }
    } else if app.selected_top_tab == 0 {
        // System tab content: top frames row (System | CPU), then GPU frame
        let sys_gfx_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(top_frames_height), // top frames area (System | CPU | Memory)
                Constraint::Length(gfx_app_height),   // Applications | GPU row (height fits larger of the two)
                Constraint::Length(disks_block_height),// Disks block
                Constraint::Min(5),                    // Process block fills remaining space
            ])
            .split(top_area);

        // Split the top frames area into three columns: System (left), CPU (middle), Memory (right)
        let top_frames_cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Ratio(1, 3),
                Constraint::Ratio(1, 3),
                Constraint::Ratio(1, 3),
            ])
            .split(sys_gfx_chunks[0]);

        // Build System info lines
        let sys_name = System::name().unwrap_or_else(|| "Unknown".to_string());
        let kernel = System::kernel_version().unwrap_or_else(|| "Unknown".to_string());
        let os_ver = System::long_os_version().unwrap_or_else(|| System::os_version().unwrap_or_else(|| "Unknown".to_string()));
        let host = System::host_name().unwrap_or_else(|| "Unknown".to_string());
        let proc_model = get_processor_model_string(sys);
        let (manu_opt, model_opt) = get_hw_manufacturer_and_model();
        let manufacturer = manu_opt.unwrap_or_else(|| "N/A".to_string());
        let hw_model = model_opt.unwrap_or_else(|| "N/A".to_string());

        let manu_line = if hw_model == "N/A" {
            format!("Manufacturer: {}", manufacturer)
        } else {
            format!("Manufacturer: {} ({})", manufacturer, hw_model)
        };

        let sys_lines = vec![
            Line::from(Span::raw(manu_line)),
            Line::from(Span::raw(format!("Processor Model: {}", proc_model))),
            Line::from(Span::raw(format!("OS Name: {}", sys_name))),
            Line::from(Span::raw(format!("Kernel version: {}", kernel))),
            Line::from(Span::raw(format!("OS version: {}", os_ver))),
            Line::from(Span::raw(format!("Hostname: {}", host))),
        ];
        let sys_par = ratatui::widgets::Paragraph::new(sys_lines)
            .block(Block::default().borders(Borders::ALL).title(" System "));
        f.render_widget(sys_par, top_frames_cols[0]);

        // Build CPU info lines for the right frame
        let threads = sys.cpus().len();
        let cores = System::physical_core_count().unwrap_or(threads);

        // CPU frame lines: basic info + usage, load averages, uptime, and temperature
        let load = System::load_average();
        let uptime_secs = System::uptime();
        let days = uptime_secs / 86_400;
        let hours = (uptime_secs % 86_400) / 3_600;
        let minutes = (uptime_secs % 3_600) / 60;
        let seconds = uptime_secs % 60;
        let uptime_str = if days > 0 {
            format!("{}d {:02}:{:02}:{:02}", days, hours, minutes, seconds)
        } else {
            format!("{:02}:{:02}:{:02}", hours, minutes, seconds)
        };
        let temp_line = match cpu_temp_c {
            Some(t) => format!("CPU Temp: {:.1}°C", t),
            None => String::from("CPU Temp: N/A"),
        };

        // Build CPU text lines (excluding CPU Usage; that will be shown as a gauge)
        let cpu_text_lines: Vec<Line> = vec![
            Line::from(Span::raw(format!("CPU Total Core: {}", cores))),
            Line::from(Span::raw(format!("CPU Thread: {}", threads))),
            Line::from(Span::raw(format!("Load Avg: {:.2} {:.2} {:.2}", load.one, load.five, load.fifteen))),
            Line::from(Span::raw(format!("Uptime: {}", uptime_str))),
            Line::from(Span::raw(temp_line)),
        ];

        // Render CPU block first, then place text and gauge inside
        let cpu_block = Block::default().borders(Borders::ALL).title(" CPU ");
        f.render_widget(cpu_block, top_frames_cols[1]);
        let cpu_inner = Rect {
            x: top_frames_cols[1].x + 1,
            y: top_frames_cols[1].y + 1,
            width: top_frames_cols[1].width.saturating_sub(2),
            height: top_frames_cols[1].height.saturating_sub(2),
        };
        if cpu_inner.width > 0 && cpu_inner.height > 0 {
            // Split inner area into: 5 lines of text + 1 line gauge
            let parts = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(5),
                    Constraint::Length(1),
                ])
                .split(cpu_inner);

            // Render the text lines
            let cpu_par = ratatui::widgets::Paragraph::new(cpu_text_lines);
            f.render_widget(cpu_par, parts[0]);

            // Render the CPU Usage row: left label + right gauge
            let ratio = (global_cpu as f64 / 100.0).clamp(0.0, 1.0);
            let row = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Length(12), // left label width
                    Constraint::Min(1),     // right gauge fills remaining
                ])
                .split(parts[1]);

            // Left label (right-aligned)
            let lbl = ratatui::widgets::Paragraph::new(Line::from(Span::raw("CPU Usage")))
                .alignment(ratatui::layout::Alignment::Right);
            f.render_widget(lbl, row[0]);

            // Right gauge without label, dynamic color thresholds
            let color = if global_cpu > 90.0 {
                Color::Red
            } else if global_cpu >= 70.0 {
                Color::Rgb(255, 191, 0)
            } else {
                Color::Green
            };
            let gauge = Gauge::default()
                .gauge_style(Style::default().fg(color))
                .ratio(ratio)
                .use_unicode(true);
            f.render_widget(gauge, row[1]);
        }

        // Build Memory info lines for the right-most frame
        let used_mem = sys.used_memory(); // KiB
        let total_mem = sys.total_memory(); // KiB
        let used_swap = sys.used_swap(); // KiB
        let total_swap = sys.total_swap(); // KiB

        // Display as MB with GB in parentheses
        let total_mem_mb = total_mem / 1024;
        let used_mem_mb = used_mem / 1024;
        let total_swap_mb = total_swap / 1024;
        let used_swap_mb = used_swap / 1024;
        let total_mem_gb = (total_mem as f64) / 1024.0 / 1024.0;
        let used_mem_gb = (used_mem as f64) / 1024.0 / 1024.0;
        let total_swap_gb = (total_swap as f64) / 1024.0 / 1024.0;
        let used_swap_gb = (used_swap as f64) / 1024.0 / 1024.0;

        let mem_lines = vec![
            Line::from(Span::raw(format!("Total RAM: {} MB ({:.1} GB)", total_mem_mb, total_mem_gb))),
            Line::from(Span::raw(format!("Used RAM: {} MB ({:.1} GB)", used_mem_mb, used_mem_gb))),
            Line::from(Span::raw(format!("Total SWAP: {} MB ({:.1} GB)", total_swap_mb, total_swap_gb))),
            Line::from(Span::raw(format!("Used SWAP: {} MB ({:.1} GB)", used_swap_mb, used_swap_gb))),
        ];
        // Render Memory block first, then place text and RAM gauge inside
        let mem_block = Block::default().borders(Borders::ALL).title(" Memory ");
        f.render_widget(mem_block, top_frames_cols[2]);
        let mem_inner = Rect {
            x: top_frames_cols[2].x + 1,
            y: top_frames_cols[2].y + 1,
            width: top_frames_cols[2].width.saturating_sub(2),
            height: top_frames_cols[2].height.saturating_sub(2),
        };
        if mem_inner.width > 0 && mem_inner.height > 0 {
            // Split inner area into: 2 lines of text + 1 line RAM gauge + 2 lines of text + 1 line SWAP gauge
            let parts = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(2), // Total RAM, Used RAM
                    Constraint::Length(1), // RAM Usage gauge row
                    Constraint::Length(2), // Total SWAP, Used SWAP
                    Constraint::Length(1), // SWAP Usage gauge row
                ])
                .split(mem_inner);

            // Render the first two text lines (Total RAM, Used RAM)
            let mem_top_par = ratatui::widgets::Paragraph::new(vec![mem_lines[0].clone(), mem_lines[1].clone()]);
            f.render_widget(mem_top_par, parts[0]);

            // Render RAM Usage row: left label + right gauge (dynamic color)
            let ram_ratio = if total_mem > 0 { (used_mem as f64 / total_mem as f64).clamp(0.0, 1.0) } else { 0.0 };
            let ram_pct = if total_mem > 0 { (used_mem as f32 / total_mem as f32) * 100.0 } else { 0.0 };
            let row_ram = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Length(12), // left label width
                    Constraint::Min(1),     // right gauge fills remaining
                ])
                .split(parts[1]);

            // Left label (right-aligned)
            let lbl_ram = ratatui::widgets::Paragraph::new(Line::from(Span::raw("RAM Usage")))
                .alignment(ratatui::layout::Alignment::Right);
            f.render_widget(lbl_ram, row_ram[0]);

            // Right gauge without label, with dynamic color thresholds
            let color_ram = if ram_pct > 90.0 {
                Color::Red
            } else if ram_pct >= 70.0 {
                Color::Rgb(255, 191, 0) // amber
            } else {
                Color::Green
            };
            let gauge_ram = Gauge::default()
                .gauge_style(Style::default().fg(color_ram))
                .ratio(ram_ratio)
                .use_unicode(true);
            f.render_widget(gauge_ram, row_ram[1]);

            // Render the next two text lines (Total SWAP, Used SWAP)
            let mem_bottom_par = ratatui::widgets::Paragraph::new(vec![mem_lines[2].clone(), mem_lines[3].clone()]);
            f.render_widget(mem_bottom_par, parts[2]);

            // Render SWAP Usage row: left label + right gauge (dynamic color)
            let swap_ratio = if total_swap > 0 { (used_swap as f64 / total_swap as f64).clamp(0.0, 1.0) } else { 0.0 };
            let swap_pct = if total_swap > 0 { (used_swap as f32 / total_swap as f32) * 100.0 } else { 0.0 };
            let row_swap = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Length(12), // left label width
                    Constraint::Min(1),     // right gauge fills remaining
                ])
                .split(parts[3]);

            // Left label (right-aligned)
            let lbl_swap = ratatui::widgets::Paragraph::new(Line::from(Span::raw("SWAP Usage")))
                .alignment(ratatui::layout::Alignment::Right);
            f.render_widget(lbl_swap, row_swap[0]);

            // Right gauge without label, with dynamic color thresholds
            let color_swap = if swap_pct > 90.0 {
                Color::Red
            } else if swap_pct >= 70.0 {
                Color::Rgb(255, 191, 0) // amber
            } else {
                Color::Green
            };
            let gauge_swap = Gauge::default()
                .gauge_style(Style::default().fg(color_swap))
                .ratio(swap_ratio)
                .use_unicode(true);
            f.render_widget(gauge_swap, row_swap[1]);
        }

        // Applications and GPU row (two columns)
        let gpu_app_cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Ratio(2, 5), // left: Applications (narrower)
                Constraint::Ratio(3, 5), // right: GPU (wider)
            ])
            .split(sys_gfx_chunks[1]);

        // Right: GPU frame
        {
            // Build lines: header + each GPU with aligned columns
            let mut lines: Vec<Line> = Vec::new();
            // Column widths
            let pci_w: usize = 12;     // e.g., 0000:01:00.0 (12 chars)
            let driver_w: usize = 12;  // e.g., amdgpu, nvidia, i915
            let vendor_w: usize = 10;  // e.g., NVIDIA, AMD, Intel
            let temp_w: usize = 8;     // e.g., 65.2°C or N/A

            // Simple truncation helper with ellipsis when needed
            let trunc = |s: &str, max: usize| -> String {
                if max == 0 { return String::new(); }
                let len = s.chars().count();
                if len <= max { return s.to_string(); }
                let keep = max.saturating_sub(1);
                let mut out = String::with_capacity(max);
                for (i, ch) in s.chars().enumerate() {
                    if i >= keep { break; }
                    out.push(ch);
                }
                out.push('…');
                out
            };

            // Header
            lines.push(Line::from(Span::styled(
                format!(
                    "{:<pci_w$}  {:<driver_w$}  {:<vendor_w$}  {:<temp_w$}  {}",
                    "PCI", "DRIVER", "VENDOR", "TEMP", "MODEL",
                    pci_w=pci_w, driver_w=driver_w, vendor_w=vendor_w, temp_w=temp_w
                ),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            )));

            if gpus.is_empty() {
                lines.push(Line::from(Span::raw("No GPUs detected")));
            } else {
                // Rows
                for g in gpus {
                    let pci = trunc(&g.pci_addr, pci_w);
                    let drv = trunc(&g.driver, driver_w);
                    let ven = trunc(&g.vendor, vendor_w);
                    let temp = match g.temp_c {
                        Some(t) => format!("{:.1}°C", t),
                        None => "N/A".to_string(),
                    };
                    let temp = trunc(&temp, temp_w);
                    let line = Line::from(Span::raw(format!(
                        "{:<pci_w$}  {:<driver_w$}  {:<vendor_w$}  {:<temp_w$}  {}",
                        pci, drv, ven, temp, g.model,
                        pci_w=pci_w, driver_w=driver_w, vendor_w=vendor_w, temp_w=temp_w
                    )));
                    lines.push(line);
                }
            }
            let gfx_par = ratatui::widgets::Paragraph::new(lines)
                .block(Block::default().borders(Borders::ALL).title(" GPU "));
            f.render_widget(gfx_par, gpu_app_cols[1]);
        }

        // Left: Applications frame (moved here from bottom row)
        {
            let app_outer = gpu_app_cols[0];
            let app_block = Block::default().borders(Borders::ALL).title(" Applications ");
            f.render_widget(app_block, app_outer);
            let app_inner = Rect {
                x: app_outer.x + 1,
                y: app_outer.y + 1,
                width: app_outer.width.saturating_sub(2),
                height: app_outer.height.saturating_sub(2),
            };
            if app_inner.width > 0 && app_inner.height > 0 {
                // Build rows for Applications table
                let rows_data = build_applications_status();
                let header = Row::new(vec![
                    Cell::from(Span::styled("Application", Style::default().add_modifier(Modifier::BOLD))),
                    Cell::from(Span::styled("Active", Style::default().add_modifier(Modifier::BOLD))),
                    Cell::from(Span::styled("Installed", Style::default().add_modifier(Modifier::BOLD))),
                ]);
                let mut rows: Vec<Row> = Vec::new();
                for (name, active, installed) in rows_data {
                    let active_style = if active == "Yes" { Style::default().fg(Color::Green) } else if active == "No" { Style::default().fg(Color::Red) } else { Style::default() };
                    let inst_style = if installed == "Yes" { Style::default().fg(Color::Green) } else { Style::default().fg(Color::Red) };
                    rows.push(Row::new(vec![
                        Cell::from(Span::raw(name)),
                        Cell::from(Span::styled(active, active_style)),
                        Cell::from(Span::styled(installed, inst_style)),
                    ]));
                }
                if rows.is_empty() {
                    rows.push(Row::new(vec![
                        Cell::from(Span::raw("No applications.")),
                        Cell::from(Span::raw("")),
                        Cell::from(Span::raw("")),
                    ]));
                }
                // Column widths: name flex, Active 8, Installed 10
                let table = Table::new(
                    rows,
                    [
                        Constraint::Min(10),
                        Constraint::Length(8),
                        Constraint::Length(10),
                    ],
                )
                .header(header)
                .block(Block::default());
                f.render_widget(table, app_inner);
            }
        }

        // Disks and Network area (two columns)
        let disks_row_cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Ratio(1, 2), // left: Disks
                Constraint::Ratio(1, 2), // right: Network
            ])
            .split(sys_gfx_chunks[2]);

        // Left column: Disks frame with table
        let mut dlines: Vec<Line> = Vec::new();
        // Column widths
        let dev_w: usize = 14;   // e.g., /dev/nvme0n1
        let mnt_w: usize = 18;   // mount point
        let fs_w: usize = 8;     // ext4/xfs/btrfs
        let size_w: usize = 8;   // GiB formatted
        let used_w: usize = 8;   // GiB formatted
        let pct_w: usize = 6;    // 100.0%
        let temp_w: usize = 8;   // 55.2°C
        let trunc = |s: &str, max: usize| -> String {
            if max == 0 { return String::new(); }
            let len = s.chars().count();
            if len <= max { return s.to_string(); }
            let keep = max.saturating_sub(1);
            let mut out = String::with_capacity(max);
            for (i, ch) in s.chars().enumerate() { if i >= keep { break; } out.push(ch); }
            out.push('…');
            out
        };
        // Header
        dlines.push(Line::from(Span::styled(
            format!(
                "{:<dev_w$}  {:<mnt_w$}  {:<fs_w$}  {:>size_w$}  {:>used_w$}  {:>pct_w$}  {:<temp_w$}",
                "DEVICE", "MOUNT", "FS", "TOTAL", "USED", "%USED", "TEMP",
                dev_w=dev_w, mnt_w=mnt_w, fs_w=fs_w, size_w=size_w, used_w=used_w, pct_w=pct_w, temp_w=temp_w
            ),
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        )));
        if disks.is_empty() {
            dlines.push(Line::from(Span::raw("No disks detected (or unsupported OS)")));
        } else {
            for d in &disks {
                let dev = trunc(&d.dev, dev_w);
                let mnt = trunc(&d.mount, mnt_w);
                let fs = trunc(&d.fs, fs_w);
                let total = fmt_bytes_gib(d.total);
                let used = fmt_bytes_gib(d.used);
                let pct = format!("{:.1}%", d.pct);
                let temp = match d.temp_c { Some(t) => format!("{:.1}°C", t), None => "N/A".to_string() };
                let temp = trunc(&temp, temp_w);
                dlines.push(Line::from(Span::raw(format!(
                    "{:<dev_w$}  {:<mnt_w$}  {:<fs_w$}  {:>size_w$}  {:>used_w$}  {:>pct_w$}  {:<temp_w$}",
                    dev, mnt, fs, total, used, pct, temp,
                    dev_w=dev_w, mnt_w=mnt_w, fs_w=fs_w, size_w=size_w, used_w=used_w, pct_w=pct_w, temp_w=temp_w
                ))));
            }
        }
        let disks_par = ratatui::widgets::Paragraph::new(dlines)
            .block(Block::default().borders(Borders::ALL).title(" Disks "));
        f.render_widget(disks_par, disks_row_cols[0]);

        // Right column: Network frame with live Tx/Rx speeds
        let net_outer = disks_row_cols[1];
        let net_block = Block::default().borders(Borders::ALL).title(" Network ");
        f.render_widget(net_block, net_outer);
        let net_inner = Rect {
            x: net_outer.x + 1,
            y: net_outer.y + 1,
            width: net_outer.width.saturating_sub(2),
            height: net_outer.height.saturating_sub(2),
        };
        if net_inner.width > 0 && net_inner.height > 0 {
            // Build header + interface rows
            let mut lines: Vec<Line> = Vec::new();
            let if_w: usize = 12;
            let col_w: usize = 12; // RX/s and TX/s columns
            lines.push(Line::from(Span::styled(
                format!(
                    "{:<if_w$}  {:>col_w$}  {:>col_w$}",
                    "IFACE", "RX/s", "TX/s", if_w=if_w, col_w=col_w
                ),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            )));

            // Helper to format bytes/sec
            let fmt_rate = |r: f64| -> String {
                let mut v = r;
                let units = ["B/s", "KiB/s", "MiB/s", "GiB/s", "TiB/s"];
                let mut i = 0;
                while v >= 1024.0 && i + 1 < units.len() { v /= 1024.0; i += 1; }
                if v >= 100.0 { format!("{:.0} {}", v, units[i]) }
                else if v >= 10.0 { format!("{:.1} {}", v, units[i]) }
                else { format!("{:.2} {}", v, units[i]) }
            };

            // Sort interfaces by name for stable display
            let mut entries: Vec<(String, (f64, f64))> = app
                .net_rates
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));

            if entries.is_empty() {
                lines.push(Line::from(Span::raw("No network data (unsupported OS or no interfaces)")));
            } else {
                for (iface, (rxps, txps)) in entries {
                    let rx_s = fmt_rate(rxps);
                    let tx_s = fmt_rate(txps);
                    let iname = if iface.chars().count() > if_w { format!("{}…", &iface[..iface.char_indices().nth(if_w.saturating_sub(1)).map(|(i,_)| i).unwrap_or(0)]) } else { iface };
                    lines.push(Line::from(Span::raw(format!(
                        "{:<if_w$}  {:>col_w$}  {:>col_w$}",
                        iname, rx_s, tx_s, if_w=if_w, col_w=col_w
                    ))));
                }
            }
            let net_par = ratatui::widgets::Paragraph::new(lines);
            f.render_widget(net_par, net_inner);
        }


    } else if app.selected_top_tab == 2 {
        // Services tab
        let block = Block::default().borders(Borders::ALL).title(" Services (SystemD) ");
        f.render_widget(block, top_area);
        let inner = Rect { x: top_area.x + 1, y: top_area.y + 1, width: top_area.width.saturating_sub(2), height: top_area.height.saturating_sub(2) };
        if inner.width > 0 && inner.height > 0 {
            #[cfg(target_os = "linux")]
            {
                let services = get_all_services();
                // Clamp selection within available items (compute effective selection)
                let total = services.len();
                let selected = app.services_selected.min(total.saturating_sub(1));
                // Compute visible window based on selection and available height (minus header row)
                let rows_per_page = inner.height.saturating_sub(1) as usize;
                let max_start = total.saturating_sub(rows_per_page);
                let mut start = app.services_scroll.min(max_start);
                if selected < start { start = selected; }
                if rows_per_page > 0 && selected >= start + rows_per_page { start = selected + 1 - rows_per_page; }
                // Build header and rows
                let header = Row::new(vec![
                    Cell::from(Span::styled("UNIT", Style::default().add_modifier(Modifier::BOLD))),
                    Cell::from(Span::styled("ACTIVE", Style::default().add_modifier(Modifier::BOLD))),
                    Cell::from(Span::styled("DESCRIPTION", Style::default().add_modifier(Modifier::BOLD))),
                ]);
                let mut rows: Vec<Row> = Vec::new();
                for (i, (unit, active, desc)) in services.into_iter().skip(start).take(rows_per_page).enumerate() {
                    let unit_style = if active == "active" { Style::default().fg(Color::Green) } else { Style::default().fg(Color::Red) };
                    let mut row = Row::new(vec![
                        Cell::from(Span::styled(unit, unit_style)),
                        Cell::from(Span::raw(active)),
                        Cell::from(Span::raw(desc)),
                    ]);
                    if start + i == selected {
                        row = row.style(Style::default().add_modifier(Modifier::REVERSED));
                    }
                    rows.push(row);
                }
                if rows.is_empty() {
                    rows.push(Row::new(vec![
                        Cell::from(Span::raw("No services found.")),
                        Cell::from(Span::raw("")),
                        Cell::from(Span::raw("")),
                    ]));
                }
                // Calculate column widths: UNIT ~ 40, ACTIVE ~ 10, DESCRIPTION fills rest
                let unit_w = 40u16.min(inner.width.saturating_sub(12));
                let table = Table::new(
                    rows,
                    [
                        Constraint::Length(unit_w),
                        Constraint::Length(10),
                        Constraint::Min(10),
                    ],
                )
                .header(header)
                .block(Block::default());
                f.render_widget(table, inner);
            }
            #[cfg(not(target_os = "linux"))]
            {
                let msg = vec![Line::from(Span::raw("Services listing is supported on Linux only."))];
                let paragraph = ratatui::widgets::Paragraph::new(msg);
                f.render_widget(paragraph, inner);
            }
        }
    } else if app.selected_top_tab == 3 {
        // Shell tab
        let block = Block::default().borders(Borders::ALL).title(" Shell ");
        f.render_widget(block, top_area);
        let inner = Rect { x: top_area.x + 1, y: top_area.y + 1, width: top_area.width.saturating_sub(2), height: top_area.height.saturating_sub(2) };
        if inner.width > 0 && inner.height > 0 {
            // Split into a 1-line prompt bar and the PTY content area
            let parts = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1), // prompt line
                    Constraint::Min(1),    // PTY content
                ])
                .split(inner);
            // Render prompt bar
            let prompt = build_system_prompt();
            let prompt_line = Line::from(Span::styled(format!(" {} ", prompt), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)));
            let prompt_par = ratatui::widgets::Paragraph::new(prompt_line);
            f.render_widget(prompt_par, parts[0]);

            // Render PTY content beneath the prompt
            let content_area = parts[1];
            if let Some(sess) = app.shell.as_ref() {
                let rows = content_area.height.max(1);
                let cols = content_area.width.max(1);
                let lines = sess.read_stripped_lines(cols as usize, rows as usize);
                let text_lines: Vec<Line> = lines.into_iter().map(|l| Line::from(Span::raw(l))).collect();
                let paragraph = ratatui::widgets::Paragraph::new(text_lines);
                f.render_widget(paragraph, content_area);
            } else {
                let msg = vec![
                    Line::from(Span::raw("Press F12 to start the shell.")),
                ];
                let paragraph = ratatui::widgets::Paragraph::new(msg);
                f.render_widget(paragraph, content_area);
            }
        }
    } else if app.selected_top_tab == 4 {
        // Logs tab
        let block = Block::default().borders(Borders::ALL).title(" Logs ");
        f.render_widget(block, top_area);
        let inner = Rect { x: top_area.x + 1, y: top_area.y + 1, width: top_area.width.saturating_sub(2), height: top_area.height.saturating_sub(2) };
        if inner.width > 0 && inner.height > 0 {
            let files = list_var_log_files();
            let total = files.len();
            let selected = app.logs_selected.min(total.saturating_sub(1));
            let rows_per_page = inner.height.saturating_sub(1) as usize;
            let max_start = total.saturating_sub(rows_per_page);
            let mut start = app.logs_scroll.min(max_start);
            if selected < start { start = selected; }
            if rows_per_page > 0 && selected >= start + rows_per_page { start = selected + 1 - rows_per_page; }
            // Header
            let header = Row::new(vec![
                Cell::from(Span::styled("NAME", Style::default().add_modifier(Modifier::BOLD))),
                Cell::from(Span::styled("SIZE", Style::default().add_modifier(Modifier::BOLD))),
                Cell::from(Span::styled("MODIFIED", Style::default().add_modifier(Modifier::BOLD))),
            ]);
            let mut rows: Vec<Row> = Vec::new();
            for (i, ent) in files.into_iter().skip(start).take(rows_per_page).enumerate() {
                let mut row = Row::new(vec![
                    Cell::from(Span::raw(ent.name)),
                    Cell::from(Span::raw(fmt_bytes(ent.size))),
                    Cell::from(Span::raw(ent.modified)),
                ]);
                if start + i == selected { row = row.style(Style::default().add_modifier(Modifier::REVERSED)); }
                rows.push(row);
            }
            if rows.is_empty() {
                rows.push(Row::new(vec![
                    Cell::from(Span::raw("No log files.")),
                    Cell::from(Span::raw("")),
                    Cell::from(Span::raw("")),
                ]));
            }
            // Column widths: Name fills, Size 12, Modified 16
            let table = Table::new(
                rows,
                [
                    Constraint::Min(10),
                    Constraint::Length(12),
                    Constraint::Length(16),
                ],
            )
            .header(header)
            .block(Block::default());
            f.render_widget(table, inner);
        }
    }
}


/// Draw the function key menu bar (F1..F10) along the bottom.
fn draw_menu(f: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    // Menu bar with function keys
    // F2: Dashboard, F3: top/htop, F4: Services (SystemD), F5: Logs, F10: Exit, F12: Shell
    // Fill background with a lighter blue for the entire menu area
    let bg = Block::default().style(Style::default().bg(Color::LightBlue));
    f.render_widget(bg, area);

    // Base styles
    let key_style = Style::default().fg(Color::White).bg(Color::Black).add_modifier(Modifier::BOLD);
    let hint_style = Style::default().fg(Color::Black);

    // Active styles for the currently selected tab's menu item
    let key_style_active = Style::default().fg(Color::Yellow).bg(Color::Black).add_modifier(Modifier::BOLD);
    let hint_style_active = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);

    // Determine which menu item to highlight based on active tab
    let dash_active = app.selected_top_tab == 0;
    let top_active = app.selected_top_tab == 1;
    let services_active = app.selected_top_tab == 2;
    let logs_active = app.selected_top_tab == 4;
    let shell_active = app.selected_top_tab == 3;

    let text = Line::from(vec![
        // F1 Help (not a tab)
        Span::styled(" F1 ", key_style), Span::styled(" Help  ", hint_style),
        // F2 Dashboard
        Span::styled(" F2 ", if dash_active { key_style_active } else { key_style }),
        Span::styled(" Dashboard  ", if dash_active { hint_style_active } else { hint_style }),
        // F3 top/htop
        Span::styled(" F3 ", if top_active { key_style_active } else { key_style }),
        Span::styled(" top/htop  ", if top_active { hint_style_active } else { hint_style }),
        // F4 Services (SystemD)
        Span::styled(" F4 ", if services_active { key_style_active } else { key_style }),
        Span::styled(" Services (SystemD)  ", if services_active { hint_style_active } else { hint_style }),
        // F5 Logs
        Span::styled(" F5 ", if logs_active { key_style_active } else { key_style }),
        Span::styled(" Logs  ", if logs_active { hint_style_active } else { hint_style }),
        Span::styled(" F6 ", key_style), Span::styled("        ", hint_style),
        Span::styled(" F7 ", key_style), Span::styled("        ", hint_style),
        Span::styled(" F8 ", key_style), Span::styled("        ", hint_style),
        Span::styled(" F9 ", key_style), Span::styled("        ", hint_style),
        // F10 Exit
        Span::styled(" F10 ", key_style), Span::styled(" Exit  ", Style::default().fg(Color::Black).add_modifier(Modifier::BOLD)),
        // F11 unmapped
        Span::styled(" F11 ", key_style), Span::styled("        ", hint_style),
        // F12 Shell tab
        Span::styled(" F12 ", if shell_active { key_style_active } else { key_style }),
        Span::styled(" Shell  ", if shell_active { hint_style_active } else { hint_style }),
    ]);
    let paragraph = ratatui::widgets::Paragraph::new(text);
    f.render_widget(paragraph, area);
}


// Help popup drawing (F1)
/// Draw the F1 Help popup with multiline content and a cyan border + shadow.
fn draw_help_popup(f: &mut ratatui::Frame<'_>, size: Rect) {
    // Build help text lines (multiline with indentation)
    let lines = vec![
        Line::from(Span::raw(format!("{} v {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")))),
        Line::from(Span::raw(format!("© {} {}", env!("RTOP_COPYRIGHT_YEAR"), env!("CARGO_PKG_AUTHORS")))), 
        Line::from(Span::raw(format!("License: {}", env!("CARGO_PKG_LICENSE")))),
        Line::from(Span::raw(" ")),
        Line::from(Span::raw("Navigation and hotkeys:")),
        Line::from(Span::raw("    - Switch tabs: Left/Right, h/l, Tab/BackTab, or 1/2/3/4/5.")),
        Line::from(Span::raw("    - In tables (Processes/Services/Logs): Home/End jump to first/last; PgUp/PgDn move by 10.")),
        Line::from(Span::raw("    - F2 Dashboard, F3 top/htop, F4 Services (SystemD), F5 Logs, F12 Shell (embedded PTY).")),
        Line::from(Span::raw("    - In Shell tab: keys go to your shell; Ctrl-C is sent to the shell (F10 exits app).")), 
        Line::from(Span::raw("    - F10 exit app; q also exits.")), 
    ];

    // Compute popup width: max text width + 1 space padding + 4 (borders)
    // Use Line::width() to account for unicode character widths.
    let max_text_width: u16 = lines
        .iter()
        .map(|l| l.width() as u16)
        .max()
        .unwrap_or(0)
        .saturating_add(1); // left pad of 1 space for all rows
    let mut popup_w: u16 = max_text_width.saturating_add(4);
    if popup_w > size.width { popup_w = size.width; }

    // Dynamic height based on number of lines (+2 for borders)
    let mut popup_h: u16 = (lines.len() as u16).saturating_add(2);
    if popup_h > size.height { popup_h = size.height; }

    // Center the popup
    let popup_x = size.x + (size.width.saturating_sub(popup_w)) / 2;
    let popup_y = size.y + (size.height.saturating_sub(popup_h)) / 2;
    let area = Rect { x: popup_x, y: popup_y, width: popup_w, height: popup_h };

    // Draw shadow first: offset by (1,1), clamped to screen
    let sx = area.x.saturating_add(1);
    let sy = area.y.saturating_add(1);
    if sx < size.x + size.width && sy < size.y + size.height {
        let sw = area.width.min((size.x + size.width).saturating_sub(sx));
        let sh = area.height.min((size.y + size.height).saturating_sub(sy));
        if sw > 0 && sh > 0 {
            let shadow = Rect { x: sx, y: sy, width: sw, height: sh };
            let shadow_block = Block::default().style(Style::default().bg(Color::Black));
            f.render_widget(shadow_block, shadow);
        }
    }

    // Clear and border for the main popup
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Help ")
        .border_style(Style::default().fg(Color::Cyan));
    f.render_widget(block, area);

    // Inner text area
    let inner = Rect { x: area.x + 1, y: area.y + 1, width: area.width.saturating_sub(2), height: area.height.saturating_sub(2) };
    // Prepend one leading space to every rendered line
    let padded_lines: Vec<Line> = lines
        .into_iter()
        .map(|l| {
            let mut spans = vec![Span::raw(" ")];
            spans.extend(l.spans);
            Line::from(spans)
        })
        .collect();
    let paragraph = ratatui::widgets::Paragraph::new(padded_lines);
    f.render_widget(paragraph, inner);
}

/// Draw the Service Details popup with a dynamic title and multi-line body text.
fn draw_service_popup(f: &mut ratatui::Frame<'_>, size: Rect, title: &str, text: &str) {
    // Split text into lines and measure width
    let lines_raw: Vec<&str> = text.split('\n').collect();
    let max_text_width: u16 = lines_raw.iter().map(|l| l.chars().count() as u16).max().unwrap_or(0).saturating_add(1);
    // Add some padding and clamp to screen bounds
    let mut popup_w: u16 = max_text_width.saturating_add(4);
    if popup_w > size.width { popup_w = size.width; }
    // Height is based on number of lines (capped to screen)
    let mut popup_h: u16 = (lines_raw.len() as u16).saturating_add(2);
    if popup_h > size.height { popup_h = size.height; }

    // Center the popup
    let popup_x = size.x + (size.width.saturating_sub(popup_w)) / 2;
    let popup_y = size.y + (size.height.saturating_sub(popup_h)) / 2;
    let area = Rect { x: popup_x, y: popup_y, width: popup_w, height: popup_h };

    // Shadow
    let sx = area.x.saturating_add(1);
    let sy = area.y.saturating_add(1);
    if sx < size.x + size.width && sy < size.y + size.height {
        let sw = area.width.min((size.x + size.width).saturating_sub(sx));
        let sh = area.height.min((size.y + size.height).saturating_sub(sy));
        if sw > 0 && sh > 0 {
            let shadow = Rect { x: sx, y: sy, width: sw, height: sh };
            let shadow_block = Block::default().style(Style::default().bg(Color::Black));
            f.render_widget(shadow_block, shadow);
        }
    }

    // Border and clear
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Service: {} ", title))
        .border_style(Style::default().fg(Color::Green));
    f.render_widget(block, area);

    // Inner text area
    let inner = Rect { x: area.x + 1, y: area.y + 1, width: area.width.saturating_sub(2), height: area.height.saturating_sub(2) };
    let lines: Vec<Line> = lines_raw.into_iter().map(|l| Line::from(Span::raw(format!(" {}", l)))).collect();
    let paragraph = ratatui::widgets::Paragraph::new(lines);
    f.render_widget(paragraph, inner);
}



/// Draw the Process Details popup with a dynamic title and multi-line body text.
fn draw_process_popup(f: &mut ratatui::Frame<'_>, size: Rect, title: &str, text: &str) {
    // Split text into lines and measure width
    let lines_raw: Vec<&str> = text.split('\n').collect();
    let max_text_width: u16 = lines_raw.iter().map(|l| l.chars().count() as u16).max().unwrap_or(0).saturating_add(1);
    // Add some padding and clamp to screen bounds
    let mut popup_w: u16 = max_text_width.saturating_add(4);
    if popup_w > size.width { popup_w = size.width; }
    // Height is based on number of lines (capped to screen)
    let mut popup_h: u16 = (lines_raw.len() as u16).saturating_add(2);
    if popup_h > size.height { popup_h = size.height; }

    // Center the popup
    let popup_x = size.x + (size.width.saturating_sub(popup_w)) / 2;
    let popup_y = size.y + (size.height.saturating_sub(popup_h)) / 2;
    let area = Rect { x: popup_x, y: popup_y, width: popup_w, height: popup_h };

    // Shadow
    let sx = area.x.saturating_add(1);
    let sy = area.y.saturating_add(1);
    if sx < size.x + size.width && sy < size.y + size.height {
        let sw = area.width.min((size.x + size.width).saturating_sub(sx));
        let sh = area.height.min((size.y + size.height).saturating_sub(sy));
        if sw > 0 && sh > 0 {
            let shadow = Rect { x: sx, y: sy, width: sw, height: sh };
            let shadow_block = Block::default().style(Style::default().bg(Color::Black));
            f.render_widget(shadow_block, shadow);
        }
    }

    // Border and clear
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Process: {} ", title))
        .border_style(Style::default().fg(Color::Yellow));
    f.render_widget(block, area);

    // Inner text area
    let inner = Rect { x: area.x + 1, y: area.y + 1, width: area.width.saturating_sub(2), height: area.height.saturating_sub(2) };
    let lines: Vec<Line> = lines_raw.into_iter().map(|l| Line::from(Span::raw(format!(" {}", l)))).collect();
    let paragraph = ratatui::widgets::Paragraph::new(lines);
    f.render_widget(paragraph, inner);
}

/// Draw the Log Details popup with a dynamic title and multi-line body text.
fn draw_log_popup(f: &mut ratatui::Frame<'_>, size: Rect, title: &str, text: &str) {
    // Split text into lines and measure width
    let lines_raw: Vec<&str> = text.split('\n').collect();
    let max_text_width: u16 = lines_raw.iter().map(|l| l.chars().count() as u16).max().unwrap_or(0).saturating_add(1);
    let mut popup_w: u16 = max_text_width.saturating_add(4);
    if popup_w > size.width { popup_w = size.width; }
    let mut popup_h: u16 = (lines_raw.len() as u16).saturating_add(2);
    if popup_h > size.height { popup_h = size.height; }
    let popup_x = size.x + (size.width.saturating_sub(popup_w)) / 2;
    let popup_y = size.y + (size.height.saturating_sub(popup_h)) / 2;
    let area = Rect { x: popup_x, y: popup_y, width: popup_w, height: popup_h };
    let sx = area.x.saturating_add(1);
    let sy = area.y.saturating_add(1);
    if sx < size.x + size.width && sy < size.y + size.height {
        let sw = area.width.min((size.x + size.width).saturating_sub(sx));
        let sh = area.height.min((size.y + size.height).saturating_sub(sy));
        if sw > 0 && sh > 0 { let shadow = Rect { x: sx, y: sy, width: sw, height: sh }; let shadow_block = Block::default().style(Style::default().bg(Color::Black)); f.render_widget(shadow_block, shadow); }
    }
    f.render_widget(Clear, area);
    let block = Block::default().borders(Borders::ALL).title(format!(" Log: {} ", title)).border_style(Style::default().fg(Color::LightBlue));
    f.render_widget(block, area);
    let inner = Rect { x: area.x + 1, y: area.y + 1, width: area.width.saturating_sub(2), height: area.height.saturating_sub(2) };
    let lines: Vec<Line> = lines_raw.into_iter().map(|l| Line::from(Span::raw(format!(" {}", l)))).collect();
    let paragraph = ratatui::widgets::Paragraph::new(lines);
    f.render_widget(paragraph, inner);
}

/// Draw a sudo password prompt popup for Logs, with masked input and error line.
fn draw_logs_password_prompt(f: &mut ratatui::Frame<'_>, size: Rect, error_text: &str, chars_len: usize) {
    let lines = vec![
        Line::from(Span::raw("Enter sudo password to read protected logs:")),
        Line::from(Span::raw(" ")), // spacer
        Line::from(Span::raw("Password: ")), // input line label
        Line::from(Span::raw(" ")), // spacer
        Line::from(Span::styled(error_text.to_string(), Style::default().fg(Color::Red))),
    ];
    // Fixed width popup
    let mut popup_w: u16 = 60;
    if popup_w > size.width { popup_w = size.width; }
    let mut popup_h: u16 = 7;
    if popup_h > size.height { popup_h = size.height; }
    let popup_x = size.x + (size.width.saturating_sub(popup_w)) / 2;
    let popup_y = size.y + (size.height.saturating_sub(popup_h)) / 2;
    let area = Rect { x: popup_x, y: popup_y, width: popup_w, height: popup_h };
    let sx = area.x.saturating_add(1);
    let sy = area.y.saturating_add(1);
    if sx < size.x + size.width && sy < size.y + size.height {
        let sw = area.width.min((size.x + size.width).saturating_sub(sx));
        let sh = area.height.min((size.y + size.height).saturating_sub(sy));
        if sw > 0 && sh > 0 { let shadow = Rect { x: sx, y: sy, width: sw, height: sh }; let shadow_block = Block::default().style(Style::default().bg(Color::Black)); f.render_widget(shadow_block, shadow); }
    }
    f.render_widget(Clear, area);
    let block = Block::default().borders(Borders::ALL).title(" Sudo Password ").border_style(Style::default().fg(Color::Magenta));
    f.render_widget(block, area);
    // Inner
    let inner = Rect { x: area.x + 1, y: area.y + 1, width: area.width.saturating_sub(2), height: area.height.saturating_sub(2) };
    let mut display_lines: Vec<Line> = Vec::new();
    display_lines.push(lines[0].clone());
    display_lines.push(lines[1].clone());
    // Render password masked line
    let masked = "*".repeat(chars_len);
    display_lines.push(Line::from(Span::raw(format!("Password: {}", masked))));
    display_lines.push(lines[3].clone());
    display_lines.push(lines[4].clone());
    let paragraph = ratatui::widgets::Paragraph::new(display_lines);
    f.render_widget(paragraph, inner);
}


