// -------- Process details helpers (Linux best-effort) --------
#[cfg(target_os = "linux")]
pub fn get_process_name(pid: i32) -> String {
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
pub fn get_process_name(_pid: i32) -> String { String::new() }

#[cfg(target_os = "linux")]
pub fn get_process_details(pid: i32) -> String {
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
    } else { out.push_str("(status not available)\n"); }

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
    if let Ok(rd) = std::fs::read_dir(&fd_dir) { let count = rd.filter(|e| e.is_ok()).count(); out.push_str(&format!("FDs: {}\n", count)); }

    if out.is_empty() { out = String::from("(no details)"); }
    out
}

#[cfg(not(target_os = "linux"))]
pub fn get_process_details(pid: i32) -> String { format!("Process details are supported on Linux only. PID {}", pid) }
