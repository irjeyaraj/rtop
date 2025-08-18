use std::process::Command;

// Fetch all services in a simple parsed form (Linux)
#[cfg(target_os = "linux")]
pub fn get_all_services() -> Vec<(String, String, String)> {
    let mut services: Vec<(String, String, String)> = Vec::new();
    // Use systemctl list-units --type=service --all --no-legend --no-pager
    if let Ok(out) = Command::new("systemctl")
        .args(["list-units", "--type=service", "--all", "--no-legend", "--no-pager"]) 
        .output() {
        if out.status.success() {
            if let Ok(s) = String::from_utf8(out.stdout) {
                for line in s.lines() {
                    let l = line.trim();
                    if l.is_empty() { continue; }
                    // Typical columns: UNIT LOAD ACTIVE SUB DESCRIPTION
                    // We only need UNIT, ACTIVE, DESCRIPTION (join remainder)
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
pub fn get_all_services() -> Vec<(String, String, String)> { Vec::new() }

// Fetch detailed status text for a service (Linux)
#[cfg(target_os = "linux")]
pub fn get_service_status(unit: &str) -> String {
    let output = Command::new("systemctl")
        .args(["status", unit, "--no-pager", "--full"]) 
        .output();
    match output {
        Ok(out) if out.status.success() => String::from_utf8(out.stdout).unwrap_or_else(|_| String::from("(failed to decode output)")),
        Ok(out) => {
            let mut s = String::new();
            if !out.stdout.is_empty() { s = String::from_utf8_lossy(&out.stdout).into_owned(); }
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
pub fn get_service_status(_unit: &str) -> String { String::from("Service details are supported on Linux only.") }
