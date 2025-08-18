use std::process::{Command, Stdio};

#[derive(Clone)]
pub struct LogEntry { pub name: String, pub path: String, pub size: u64, pub modified: String }

pub fn list_var_log_files() -> Vec<LogEntry> {
    use std::fs;
    use std::path::{Path, PathBuf};
    let root = Path::new("/var/log");
    let mut out: Vec<LogEntry> = Vec::new();
    let mut stack: Vec<PathBuf> = Vec::new();
    // Seed with immediate children of /var/log
    if let Ok(rd) = fs::read_dir(root) {
        for ent in rd.flatten() { stack.push(ent.path()); }
    } else { return out; }

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
                for ent in rd.flatten() { stack.push(ent.path()); }
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

pub fn list_journal_files() -> Vec<LogEntry> {
    use std::fs;
    use std::path::{Path, PathBuf};
    let root = Path::new("/var/log/journal");
    let mut out: Vec<LogEntry> = Vec::new();
    if !root.exists() { return out; }
    let mut stack: Vec<PathBuf> = Vec::new();
    if let Ok(rd) = fs::read_dir(root) {
        for ent in rd.flatten() { stack.push(ent.path()); }
    } else { return out; }
    while let Some(p) = stack.pop() {
        let md = match fs::symlink_metadata(&p) { Ok(m) => m, Err(_) => continue };
        if md.is_file() {
            let name = match p.strip_prefix(root) {
                Ok(rel) => rel.to_string_lossy().to_string(),
                Err(_) => p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| p.to_string_lossy().to_string()),
            };
            let size = md.len();
            let mstr = md.modified().ok().map(fmt_system_time).unwrap_or_else(|| String::from("-"));
            out.push(LogEntry { name, path: p.to_string_lossy().to_string(), size, modified: mstr });
        } else if md.is_dir() {
            if md.file_type().is_symlink() { continue; }
            if let Ok(rd) = fs::read_dir(&p) {
                for ent in rd.flatten() { stack.push(ent.path()); }
            }
        } else if md.file_type().is_symlink() {
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
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub fn fmt_system_time(st: std::time::SystemTime) -> String {
    use std::time::UNIX_EPOCH;
    match st.duration_since(UNIX_EPOCH) {
        Ok(dur) => {
            let secs = dur.as_secs();
            // Keep it simple to avoid extra deps
            format!("{}s", secs)
        }
        Err(_) => String::from("-"),
    }
}

pub fn read_log_file_best_effort(path: &str, sudo_pass: Option<&str>) -> Result<String, String> {
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
                        .spawn() { Ok(c) => c, Err(spawn_err) => return Err(format!("Failed to spawn sudo: {}", spawn_err)) };
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

pub fn read_journal_file_best_effort(path: &str, sudo_pass: Option<&str>) -> Result<String, String> {
    // Use journalctl to read entries from a specific journal file.
    // Try without sudo first.
    let output_res = Command::new("journalctl")
        .arg("--file").arg(path)
        .arg("-n").arg("5000")
        .arg("-o").arg("short-iso")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();
    let need_sudo = match output_res {
        Ok(out) => {
            if out.status.success() {
                let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
                cap_log_text(&mut text);
                return Ok(text);
            } else {
                let err = String::from_utf8_lossy(&out.stderr).to_string();
                if err.to_ascii_lowercase().contains("permission") && sudo_pass.is_some() { true }
                else { return Err(if err.trim().is_empty() { String::from("journalctl failed") } else { err }); }
            }
        }
        Err(e) => { return Err(format!("Failed to run journalctl: {}", e)); }
    };

    if need_sudo {
        if let Some(pw) = sudo_pass {
            let mut child = match Command::new("sudo")
                .arg("-S")
                .arg("--")
                .arg("journalctl")
                .arg("--file").arg(path)
                .arg("-n").arg("5000")
                .arg("-o").arg("short-iso")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn() { Ok(c) => c, Err(spawn_err) => return Err(format!("Failed to spawn sudo: {}", spawn_err)) };
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
                return Err(if err.trim().is_empty() { String::from("sudo journalctl failed") } else { err });
            }
        }
    }
    Err(String::from("Unable to read journal (no sudo password provided)"))
}

pub fn cap_log_text(s: &mut String) {
    // Keep at most last ~5000 lines to avoid huge popups
    let max_lines = 5000usize;
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() > max_lines {
        let tail = &lines[lines.len()-max_lines..];
        *s = tail.join("\n");
    }
}
