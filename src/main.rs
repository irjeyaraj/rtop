// Copyright (c) 2025 Immanuel Raja Jeyaraj <irj@sefier.com>
//! rtop: a lightweight terminal system monitor.
//! See the in-app Help (F1) for usage and hotkeys.
use std::error::Error;
use std::io;
use std::time::{Duration, Instant};
use std::sync::mpsc::{self, Receiver};
use std::io::Write as IoWrite;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Gauge, Block, Borders, Clear};
use ratatui::Terminal;
use sysinfo::{CpuRefreshKind, ProcessRefreshKind, RefreshKind, System};
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use std::io::Read;
use std::fs;

/// Global application state shared between the draw loop and input handler.
///
/// Fields capture the current UI selection and popup states, as well as PTY
/// connections used by the F9 Shell popup.
struct App {
    selected_top_tab: usize, // 0: Dashboard, 1: top/htop, 2: Shell
    #[allow(dead_code)]
    selected_proc_tab: usize, // reserved (no Process tab)
    // Terminal popup state
    term_popup: bool,
    term_buf: Vec<String>,
    term_rx: Option<Receiver<Vec<u8>>>,
    term_writer: Option<Box<dyn IoWrite + Send>>,
    term_child: Option<Box<dyn portable_pty::Child + Send>>,
    // Help popup state
    help_popup: bool,
    // Network rate tracking (iface -> (rx_bytes, tx_bytes) and computed rates in bytes/sec)
    net_prev: std::collections::HashMap<String, (u64, u64)>,
    net_rates: std::collections::HashMap<String, (f64, f64)>,
    net_last: Instant,
}

/// Construct the initial application state.
impl Default for App {
    fn default() -> Self {
        Self {
            selected_top_tab: 0,
            selected_proc_tab: 0,
            term_popup: false,
            term_buf: Vec::new(),
            term_rx: None,
            term_writer: None,
            term_child: None,
            help_popup: false,
            net_prev: std::collections::HashMap::new(),
            net_rates: std::collections::HashMap::new(),
            net_last: Instant::now(),
        }
    }
}


/// Program entry point: sets up the terminal backend, runs the app loop,
/// and restores the terminal state on exit.
fn main() -> Result<(), Box<dyn Error>> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen, crossterm::cursor::Hide)?;
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
    // Start embedded shell PTY so the Shell tab view is live when opened
    if app.term_rx.is_none() {
        let _ = start_terminal_popup(&mut app); // reuse PTY setup without showing popup
        app.term_popup = false; // ensure popup flag stays false
    }

    // Prepare sysinfo system with specific refresh kinds to be efficient
    let refresh = RefreshKind::new()
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
        // Drain terminal output from embedded shell (if running)
        if let Some(rx) = &app.term_rx {
            while let Ok(chunk) = rx.try_recv() {
                // Strip ANSI escapes and push into buffer as UTF-8 text lines
                let cleaned = strip_ansi_escapes::strip(&chunk);
                let text = String::from_utf8_lossy(&cleaned);
                for line in text.split_inclusive('\n') {
                    if line.ends_with('\n') {
                        let mut s = line.to_string();
                        if s.ends_with('\n') { s.pop(); }
                        app.term_buf.push(s);
                    } else {
                        // partial line, append as-is
                        if let Some(last) = app.term_buf.last_mut() {
                            last.push_str(line);
                        } else {
                            app.term_buf.push(line.to_string());
                        }
                    }
                }
                // Limit buffer size
                if app.term_buf.len() > 2000 {
                    let excess = app.term_buf.len() - 2000;
                    app.term_buf.drain(0..excess);
                }
            }
        }

        // Refresh data periodically
        sys.refresh_cpu();
        sys.refresh_processes();
        sys.refresh_memory();

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
        })?;

        // Handle input with non-blocking poll, but ensure a minimum tick rate
        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_millis(0));

        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if handle_key(key, &mut app)? {
                    break; // exit
                }
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

    match (key.code, key.modifiers) {
        (KeyCode::Char('q'), _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => return Ok(true),
        // Function keys hotkeys
        (KeyCode::F(10), _) => return Ok(true), // F10 exit
        (KeyCode::F(9), _) => { /* intentionally unmapped */ }
        (KeyCode::F(1), _) => { app.help_popup = !app.help_popup; } // F1 Help popup
        (KeyCode::F(2), _) => { app.selected_top_tab = 0; } // F2 Dashboard
        (KeyCode::F(3), _) => { app.selected_top_tab = 1; } // F3 top/htop
        // F4 intentionally left unmapped
        // F5 intentionally left unmapped
        // F6 intentionally left unmapped
        (KeyCode::F(11), _) => { /* intentionally unmapped */ }
        (KeyCode::F(12), _) => { app.selected_top_tab = 2; } // F12 Shell tab
        // Top tabs navigation
        (KeyCode::Home, _) => { app.selected_top_tab = 0; } // Home -> Dashboard
        (KeyCode::End, _) => { app.selected_top_tab = 2; }  // End -> last tab (Shell)
        (KeyCode::PageDown, _) => { // PgDn -> move backward (previous tab)
            app.selected_top_tab = (app.selected_top_tab + 2) % 3; // -1 mod 3
        }
        (KeyCode::PageUp, _) => { // PgUp -> move forward (next tab)
            app.selected_top_tab = (app.selected_top_tab + 1) % 3;
        }
        (KeyCode::Left, _) => {
            if app.selected_top_tab > 0 { app.selected_top_tab -= 1; }
        }
        (KeyCode::Right, _) => {
            if app.selected_top_tab < 2 { app.selected_top_tab += 1; }
        }
        (KeyCode::Tab, _) => {
            app.selected_top_tab = (app.selected_top_tab + 1) % 3;
        }
        (KeyCode::BackTab, _) => {
            app.selected_top_tab = (app.selected_top_tab + 2) % 3; // equivalent to -1 mod 3
        }
        (KeyCode::Char('1'), _) => { app.selected_top_tab = 0; }
        (KeyCode::Char('2'), _) => { app.selected_top_tab = 1; }
        (KeyCode::Char('3'), _) => { app.selected_top_tab = 2; }
        _ => {}
    }
    // After handling app hotkeys, forward typing keys to embedded shell when on Shell tab
    if app.selected_top_tab == 2 {
        match key.code {
            KeyCode::Char(_) | KeyCode::Enter | KeyCode::Backspace | KeyCode::Tab => {
                forward_key_to_pty(app, key);
            }
            _ => {}
        }
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

// Start terminal popup (spawn PTY and reader thread)
/// Open a PTY and spawn the user shell for the F9 Shell popup.
fn start_terminal_popup(app: &mut App) -> Result<(), Box<dyn Error>> {
    if app.term_popup { return Ok(()); }
    // Clear buffer and setup channel
    app.term_buf.clear();
    let (tx, rx) = mpsc::channel::<Vec<u8>>();

    // Open PTY with a reasonable default size
    let pty_system = NativePtySystem::default();
    let pair = pty_system.openpty(PtySize { rows: 30, cols: 100, pixel_width: 0, pixel_height: 0 })?;

    // Resolve default shell and spawn it
    let (prog, args) = default_shell_and_args();
    let mut cmd = CommandBuilder::new(prog);
    if !args.is_empty() { cmd.args(args); }
    #[cfg(unix)]
    {
        cmd.env("TERM", "xterm-256color");
    }
    let child = pair.slave.spawn_command(cmd)?; // Box<dyn Child>
    drop(pair.slave);

    // Reader thread
    let mut reader = pair.master.try_clone_reader()?;
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => { let _ = tx.send(buf[..n].to_vec()); },
                Err(_) => break,
            }
        }
    });

    // Writer
    let writer = pair.master.take_writer()?; // Box<dyn Write + Send>

    app.term_rx = Some(rx);
    app.term_writer = Some(writer);
    app.term_child = Some(child);
    app.term_popup = true;
    Ok(())
}

/// Close the Shell popup and terminate the child shell process.
#[allow(dead_code)]
fn stop_terminal_popup(app: &mut App) {
    app.term_popup = false;
    app.term_rx = None;
    app.term_writer = None;
    if let Some(mut ch) = app.term_child.take() {
        let _ = ch.kill();
        let _ = ch.wait();
    }
}

/// Translate a KeyEvent into terminal bytes and send them to the PTY writer.
fn forward_key_to_pty(app: &mut App, key: KeyEvent) {
    let mut bytes: Vec<u8> = Vec::new();
    match key.code {
        KeyCode::Enter => {
            // Send newline to the shell and also add a visual new line in the popup buffer
            bytes.push(b'\n');
            // Insert an empty line to visually separate the command line from its output
            app.term_buf.push(String::new());
        }
        KeyCode::Backspace => bytes.push(0x7f),
        KeyCode::Tab => bytes.push(b'\t'),
        KeyCode::Esc => bytes.push(0x1b),
        KeyCode::Left => bytes.extend_from_slice(b"\x1b[D"),
        KeyCode::Right => bytes.extend_from_slice(b"\x1b[C"),
        KeyCode::Up => bytes.extend_from_slice(b"\x1b[A"),
        KeyCode::Down => bytes.extend_from_slice(b"\x1b[B"),
        KeyCode::Home => bytes.extend_from_slice(b"\x1b[H"),
        KeyCode::End => bytes.extend_from_slice(b"\x1b[F"),
        KeyCode::PageUp => bytes.extend_from_slice(b"\x1b[5~"),
        KeyCode::PageDown => bytes.extend_from_slice(b"\x1b[6~"),
        KeyCode::Delete => bytes.extend_from_slice(b"\x1b[3~"),
        KeyCode::Insert => bytes.extend_from_slice(b"\x1b[2~"),
        KeyCode::Char(c) => {
            // Control modifier to control code
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                let lc = (c as u8).to_ascii_lowercase();
                let ctrl = lc & 0x1f;
                bytes.push(ctrl);
            } else {
                if key.modifiers.contains(KeyModifiers::ALT) {
                    bytes.push(0x1b);
                }
                if c == '\n' { bytes.push(b'\n'); } else { bytes.extend_from_slice(c.to_string().as_bytes()); }
            }
        }
        _ => {}
    }
    if !bytes.is_empty() {
        if let Some(w) = app.term_writer.as_mut() {
            let _ = w.write_all(&bytes);
            let _ = w.flush();
        }
    }
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
    let brand = sys.global_cpu_info().brand().to_string();
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
    let vendor = sys.global_cpu_info().vendor_id();
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
    let global_cpu = sys.global_cpu_info().cpu_usage(); // percent
    let used_mem = sys.used_memory(); // KiB
    let total_mem = sys.total_memory(); // KiB
    let _mem_pct = if total_mem > 0 { (used_mem as f32 / total_mem as f32) * 100.0 } else { 0.0 };

    // Split area vertically into: Top Tabs bar + content, Memory, Processes
    // CPU tab content height = number of core rows (max 8) + 2 (2 lines of text)
    let cpu_rows_for_height = sys.cpus().len().min(8) as u16;
    // CPU tab height: rows for gauges + RAM/SWAP row + 3 separator lines (top/below-mem/bottom)
    let cpu_height = cpu_rows_for_height + 4;

    // Detect GPUs for Graphics tab content sizing
    let gpus = detect_gpus();
    let gfx_height: u16 = (gpus.len() as u16 + 1 + 2).max(3); // header + rows + block borders
    let sys_block_height: u16 = 6 + 2; // 6 info lines (Manufacturer with Hardware Model inline, Processor Model, OS Name, Kernel, OS version, Hostname) + block borders
    let cpu_block_height: u16 = 6 + 2; // 6 info lines + block borders (Cores, Threads, CPU%, Load, Uptime, Temp)
    let mem_block_height: u16 = 6 + 2; // 4 info lines + 2 gauge rows (RAM, SWAP) + block borders
    let top_frames_height: u16 = sys_block_height.max(cpu_block_height).max(mem_block_height);

    // Disks info for System tab Disks frame sizing (best-effort, Linux-focused)
    let disks = list_disks_best_effort();
    let disks_block_height: u16 = (disks.len() as u16 + 1 + 2).max(3); // header + rows + borders
    let proc_block_height: u16 = 12; // Process frame fixed height (inner ~10 rows) + borders

    // Decide current top content height based on selected tab (0 = Dashboard, 1 = top/htop, 2 = Shell)
    let top_content_height = match app.selected_top_tab {
        0 => top_frames_height + gfx_height + disks_block_height + proc_block_height,
        1 => cpu_height,
        2 => cpu_height.max(5),
        _ => cpu_height,
    };

    // Layout: remove visual top Tabs bar and use full area for content; for top/htop and Shell, allow content to fill remaining space
    let constraints = if app.selected_top_tab == 1 || app.selected_top_tab == 2 || app.selected_top_tab == 0 {
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

        // Render a compact Process list at the bottom of the top/htop tab
        {
            use std::collections::HashMap;
            use std::fs;

            let proc_area = cpu_chunks[5];
            // Determine how many lines fit; leave space for header
            let available_rows = proc_area.height.saturating_sub(1) as usize;
            let mut lines: Vec<Line> = Vec::new();

            // Header
            lines.push(Line::from(Span::styled(
                format!("{:>6}  {:<8} {:>3} {:>3} {:>6} {:>7} {:>10}  {}", "PID", "USER", "PRI", "NI", "CPU%", "MEM%", "TIME", "CMD"),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            )));

            // Compute width for CMD column based on available content width
            let fixed_width = 6 + 2 + 8 + 1 + 3 + 1 + 3 + 1 + 6 + 1 + 7 + 1 + 10 + 2; // approx fixed columns + spaces
            let base_cmd_width = proc_area.width.saturating_sub(fixed_width as u16) as usize;

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

            let total_mem_kib_f = total_mem as f32;
            let mut rows: Vec<(i32, f32, f32, u64, String, String)> = sys
                .processes()
                .iter()
                .map(|(pid, p)| {
                    let pid_i = pid.as_u32() as i32;
                    let cpu = p.cpu_usage();
                    let mem_kib = p.memory();
                    let mem_pct = if total_mem_kib_f > 0.0 { (mem_kib as f32 / total_mem_kib_f) * 100.0 } else { 0.0 };
                    let secs = p.run_time();
                    let days = secs / 86_400;
                    let hours = (secs % 86_400) / 3_600;
                    let minutes = (secs % 3_600) / 60;
                    let seconds = secs % 60;
                    let time_str = if days > 0 { format!("{}d {:02}:{:02}:{:02}", days, hours, minutes, seconds) } else { format!("{:02}:{:02}:{:02}", hours, minutes, seconds) };
                    let cmd = if !p.cmd().is_empty() { p.cmd().join(" ") } else { p.name().to_string() };
                    (pid_i, cpu, mem_pct, mem_kib, time_str, cmd)
                })
                .collect();

            // Sort by CPU descending
            rows.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

            for (i, (pid, cpu, mem_pct, _mem_kib, time_str, cmd)) in rows.into_iter().enumerate() {
                if i + 1 >= available_rows { break; }

                // Resolve USER, PRI, NI via /proc files
                let user = {
                    let status_path = format!("/proc/{}/status", pid);
                    if let Ok(s) = fs::read_to_string(&status_path) {
                        if let Some(uid_line) = s.lines().find(|l| l.starts_with("Uid:")) {
                            let mut it = uid_line.split_whitespace();
                            it.next(); // skip "Uid:"
                            if let Some(uid_str) = it.next() {
                                if let Ok(uid) = uid_str.parse::<u32>() {
                                    uid_name.get(&uid).cloned().unwrap_or_else(|| uid.to_string())
                                } else { String::from("?") }
                            } else { String::from("?") }
                        } else { String::from("?") }
                    } else { String::from("?") }
                };

                let (pri, ni) = {
                    let stat_path = format!("/proc/{}/stat", pid);
                    if let Ok(s) = fs::read_to_string(&stat_path) {
                        if let Some(rparen) = s.rfind(')') {
                            let after = &s[rparen+2..]; // skip ") "
                            let toks: Vec<&str> = after.split_whitespace().collect();
                            let pri = toks.get(15).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
                            let ni = toks.get(16).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
                            (pri, ni)
                        } else { (0, 0) }
                    } else { (0, 0) }
                };

                let cmd_display = if cmd.len() > base_cmd_width && base_cmd_width > 1 { format!("{}…", &cmd[..base_cmd_width.saturating_sub(1)]) } else { cmd };
                let line = Line::from(Span::raw(format!(
                    "{:>6}  {:<8} {:>3} {:>3} {:>6.1} {:>7.1} {:>10}  {}",
                    pid, user, pri, ni, cpu, mem_pct, time_str, cmd_display
                )));
                lines.push(line);
            }

            let proc_paragraph = ratatui::widgets::Paragraph::new(lines);
            f.render_widget(proc_paragraph, proc_area);
        }
    } else if app.selected_top_tab == 0 {
        // System tab content: top frames row (System | CPU), then GPU frame
        let sys_gfx_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(top_frames_height), // top frames area (System | CPU | Memory)
                Constraint::Length(gfx_height),        // GPU block
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
        let cores = sys.physical_core_count().unwrap_or(threads);

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

        // GPU area
        let gfx_inner = sys_gfx_chunks[1];

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
            for g in &gpus {
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
        f.render_widget(gfx_par, gfx_inner);

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

        // Process area (framed)
        let proc_outer = sys_gfx_chunks[3];
        let proc_block = Block::default().borders(Borders::ALL).title(" Process ");
        f.render_widget(proc_block, proc_outer);
        let proc_inner = Rect {
            x: proc_outer.x + 1,
            y: proc_outer.y + 1,
            width: proc_outer.width.saturating_sub(2),
            height: proc_outer.height.saturating_sub(2),
        };
        if proc_inner.width > 0 && proc_inner.height > 0 {
            use std::collections::HashMap;
            use std::fs;

            let mut lines: Vec<Line> = Vec::new();
            // Header
            lines.push(Line::from(Span::styled(
                format!("{:>6}  {:<8} {:>3} {:>3} {:>6} {:>7} {:>10}  {}", "PID", "USER", "PRI", "NI", "CPU%", "MEM%", "TIME", "CMD"),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            )));

            // Width for CMD column
            let fixed_width = 6 + 2 + 8 + 1 + 3 + 1 + 3 + 1 + 6 + 1 + 7 + 1 + 10 + 2;
            let base_cmd_width = proc_inner.width.saturating_sub(fixed_width as u16) as usize;

            // uid -> username map
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

            let total_mem_kib_f = total_mem as f32;
            let mut rows: Vec<(i32, f32, f32, u64, String, String)> = sys
                .processes()
                .iter()
                .map(|(pid, p)| {
                    let pid_i = pid.as_u32() as i32;
                    let cpu = p.cpu_usage();
                    let mem_kib = p.memory();
                    let mem_pct = if total_mem_kib_f > 0.0 { (mem_kib as f32 / total_mem_kib_f) * 100.0 } else { 0.0 };
                    let secs = p.run_time();
                    let days = secs / 86_400;
                    let hours = (secs % 86_400) / 3_600;
                    let minutes = (secs % 3_600) / 60;
                    let seconds = secs % 60;
                    let time_str = if days > 0 { format!("{}d {:02}:{:02}:{:02}", days, hours, minutes, seconds) } else { format!("{:02}:{:02}:{:02}", hours, minutes, seconds) };
                    let cmd = if !p.cmd().is_empty() { p.cmd().join(" ") } else { p.name().to_string() };
                    (pid_i, cpu, mem_pct, mem_kib, time_str, cmd)
                })
                .collect();

            // Sort by CPU desc
            rows.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

            // Rows to fit inner height (minus header)
            let available_rows = proc_inner.height.saturating_sub(1) as usize;
            for (i, (pid, cpu, mem_pct, _mem_kib, time_str, cmd)) in rows.into_iter().enumerate() {
                if i + 1 >= available_rows { break; }

                // USER from /proc/[pid]/status
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

                // PRI/NI from /proc/[pid]/stat
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

                let cmd_display = if cmd.len() > base_cmd_width && base_cmd_width > 1 { format!("{}…", &cmd[..base_cmd_width.saturating_sub(1)]) } else { cmd };
                let line = Line::from(Span::raw(format!(
                    "{:>6}  {:<8} {:>3} {:>3} {:>6.1} {:>7.1} {:>10}  {}",
                    pid, user, pri, ni, cpu, mem_pct, time_str, cmd_display
                )));
                lines.push(line);
            }

            let proc_par = ratatui::widgets::Paragraph::new(lines);
            f.render_widget(proc_par, proc_inner);
        }

    } else if app.selected_top_tab == 2 {
        // Shell tab content: framed shell view with auto-scrolling
        let block = Block::default().borders(Borders::ALL).title(" Shell ");
        f.render_widget(block, top_area);
        // Inner area inside the border
        let inner = Rect { x: top_area.x + 1, y: top_area.y + 1, width: top_area.width.saturating_sub(2), height: top_area.height.saturating_sub(2) };
        if inner.width > 0 && inner.height > 0 {
            let max_lines = inner.height as usize;
            let start = app.term_buf.len().saturating_sub(max_lines);
            let slice = &app.term_buf[start..];
            let mut rendered: Vec<Line> = Vec::with_capacity(slice.len());
            for s in slice {
                rendered.push(Line::from(Span::raw(s.clone())));
            }
            let paragraph = ratatui::widgets::Paragraph::new(rendered);
            f.render_widget(paragraph, inner);
        }
    }
}


/// Draw the function key menu bar (F1..F10) along the bottom.
fn draw_menu(f: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    // Menu bar with function keys
    // F2: Dashboard, F3: top/htop, (F4/F5/F6/F7/F8/F9/F11 unmapped), F10: Exit, F12: Shell tab
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
    let shell_active = app.selected_top_tab == 2;

    let text = Line::from(vec![
        // F1 Help (not a tab)
        Span::styled(" F1 ", key_style), Span::styled(" Help  ", hint_style),
        // F2 Dashboard
        Span::styled(" F2 ", if dash_active { key_style_active } else { key_style }),
        Span::styled(" Dashboard  ", if dash_active { hint_style_active } else { hint_style }),
        // F3 top/htop
        Span::styled(" F3 ", if top_active { key_style_active } else { key_style }),
        Span::styled(" top/htop  ", if top_active { hint_style_active } else { hint_style }),
        // Unmapped
        Span::styled(" F4 ", key_style), Span::styled("        ", hint_style),
        Span::styled(" F5 ", key_style), Span::styled("        ", hint_style),
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

// Terminal popup drawing
/// Draw the F9 Shell popup with a visible cursor and scrollback from term_buf.
#[allow(dead_code)]
fn draw_terminal_popup(f: &mut ratatui::Frame<'_>, size: Rect, app: &App) {
    // Centered area occupying ~80% width, ~70% height
    let popup_w = (size.width as f32 * 0.8) as u16;
    let popup_h = (size.height as f32 * 0.7) as u16;
    let popup_x = size.x + (size.width.saturating_sub(popup_w)) / 2;
    let popup_y = size.y + (size.height.saturating_sub(popup_h)) / 2;
    let area = Rect { x: popup_x, y: popup_y, width: popup_w, height: popup_h };

    // Clear area and draw border
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Shell (F9 to close) ")
        .border_style(Style::default().fg(Color::Red))
        .style(Style::default().bg(Color::Black));
    f.render_widget(block, area);

    // Inner area
    let inner = Rect { x: area.x + 1, y: area.y + 1, width: area.width.saturating_sub(2), height: area.height.saturating_sub(2) };

    // Compute how many lines fit and take from the end of buffer
    let max_lines = inner.height as usize;
    let start = app.term_buf.len().saturating_sub(max_lines);
    let slice = &app.term_buf[start..];
    let mut rendered: Vec<Line> = Vec::with_capacity(slice.len());
    for s in slice {
        rendered.push(Line::from(Span::raw(s.clone())));
    }
    let paragraph = ratatui::widgets::Paragraph::new(rendered).style(Style::default().fg(Color::White).bg(Color::Black));
    f.render_widget(paragraph, inner);

    // Place a visible cursor at the end of the last visible line inside the popup
    let mut cur_x = inner.x;
    let mut cur_y = inner.y;
    if !slice.is_empty() {
        let last_idx = slice.len().saturating_sub(1) as u16;
        cur_y = inner.y.saturating_add(last_idx.min(inner.height.saturating_sub(1)));
        let last_line = slice.last().unwrap();
        // Approximate display width; for simplicity, count chars (may differ for wide glyphs)
        let mut w = last_line.chars().count() as u16;
        if w > inner.width.saturating_sub(1) { w = inner.width.saturating_sub(1); }
        cur_x = inner.x.saturating_add(w);
    }
    f.set_cursor_position((cur_x, cur_y));
}

// Help popup drawing (F1)
/// Draw the F1 Help popup with multiline content and a cyan border + shadow.
fn draw_help_popup(f: &mut ratatui::Frame<'_>, size: Rect) {
    // Build help text lines (multiline with indentation)
    let lines = vec![
        Line::from(Span::raw(format!("{} v {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")))),
        Line::from(Span::raw(format!("© 2025 {}", env!("CARGO_PKG_AUTHORS")))), 
        Line::from(Span::raw(format!("License: {}", env!("CARGO_PKG_LICENSE")))),
        Line::from(Span::raw(" ")),
        Line::from(Span::raw("Navigation and hotkeys:")),
        Line::from(Span::raw("    - Left/Right, Tab/BackTab, or 1/2/3 to switch top tabs.")),
        Line::from(Span::raw("    - Home Dashboard, End last tab, PgDn previous tab, PgUp next tab.")),
        Line::from(Span::raw("    - F2 Dashboard, F3 top/htop, F12 Shell tab.")), 
        Line::from(Span::raw("    - F10 exit app.")), 
        Line::from(Span::raw("    - q or Ctrl-C quits.")),
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

