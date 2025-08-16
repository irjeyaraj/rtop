use std::{env, fs, path::PathBuf};

fn main() {
    // Locate Cargo.toml
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let mut cargo_toml_path = PathBuf::from(&manifest_dir);
    cargo_toml_path.push("Cargo.toml");

    // Read Cargo.toml
    let toml_str = match fs::read_to_string(&cargo_toml_path) {
        Ok(s) => s,
        Err(_) => {
            // Fallback: keep previous hardcoded year behavior
            println!("cargo:rustc-env=RTOP_COPYRIGHT_YEAR=2025");
            return;
        }
    };

    // Parse and extract metadata year
    let year = toml::from_str::<toml::Value>(&toml_str)
        .ok()
        .and_then(|v| v.get("package").cloned())
        .and_then(|p| p.get("metadata").cloned())
        .and_then(|m| m.get("my_custom_tool").cloned())
        .and_then(|t| t.get("copyrightyear").cloned())
        .and_then(|y| y.as_str().map(|s| s.to_string()))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "2025".to_string());

    // Export as compile-time env var
    println!("cargo:rustc-env=RTOP_COPYRIGHT_YEAR={}", year);
}