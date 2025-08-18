use std::fs;

/// Basic GPU information detected from the system (Linux best-effort).
#[derive(Debug, Clone)]
pub struct GpuInfo {
    pub vendor: String,
    pub driver: String,
    pub pci_addr: String,
    pub model: String,
    pub temp_c: Option<f32>,
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
pub fn detect_gpus() -> Vec<GpuInfo> {
    use std::path::Path;
    let mut gpus: Vec<GpuInfo> = Vec::new();
    let drm_path = Path::new("/sys/class/drm");
    if !drm_path.exists() { return gpus; }
    let Ok(entries) = fs::read_dir(drm_path) else { return gpus; };
    let mut seen_cards = Vec::new();
    for ent in entries.flatten() {
        if let Some(name) = ent.file_name().to_str().map(|s| s.to_string()) {
            // Interested in primary nodes like card0, card1; skip connectors like card0-DP-1, renderD*, controlD*
            if name.starts_with("card") && name.chars().all(|c| c.is_ascii_alphanumeric()) {
                if !seen_cards.contains(&name) { seen_cards.push(name); }
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
                            if let Some(rest) = line.strip_prefix("Model:") { model = rest.trim().to_string(); break; }
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
