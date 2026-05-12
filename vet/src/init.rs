//! `vet init` — create `~/.vet/` and seed empty config files.

use std::process::ExitCode;

use vetter_core::fs_secure::create_dir_secure;
use vetter_core::known_hosts::{self, KnownHostsFile};
use vetter_core::matcher::loader::{self as allowlist, AllowlistFile};
use vetter_core::settings::{self, Settings};

pub fn run() -> ExitCode {
    let home = match std::env::var_os("HOME") {
        Some(h) => std::path::PathBuf::from(h),
        None => {
            eprintln!("error: $HOME is not set");
            return ExitCode::from(78);
        }
    };

    let vet_dir = home.join(".vet");

    if let Err(e) = create_dir_secure(&vet_dir, 0o700) {
        eprintln!("error: creating {}: {e}", vet_dir.display());
        return ExitCode::from(78);
    }
    println!("  dir: {}", vet_dir.display());

    let mut failed = false;
    for (name, result) in [
        ("allowlist.yaml", seed_allowlist(&vet_dir)),
        ("known-hosts.yaml", seed_known_hosts(&vet_dir)),
        ("settings.yaml", seed_settings(&vet_dir)),
    ] {
        let path = vet_dir.join(name);
        match result {
            Ok(false) => println!("exists:  {}", path.display()),
            Ok(true) => println!("created: {}", path.display()),
            Err(e) => {
                eprintln!("error: {e}");
                failed = true;
            }
        }
    }

    if failed {
        ExitCode::from(78)
    } else {
        ExitCode::SUCCESS
    }
}

/// Returns `Ok(true)` if created, `Ok(false)` if already present.
fn seed_allowlist(vet_dir: &std::path::Path) -> Result<bool, String> {
    let path = vet_dir.join("allowlist.yaml");
    if path.exists() {
        return Ok(false);
    }
    let file = AllowlistFile::default();
    allowlist::write_file(&path, &file).map_err(|e| e.to_string())?;
    Ok(true)
}

fn seed_known_hosts(vet_dir: &std::path::Path) -> Result<bool, String> {
    let path = vet_dir.join("known-hosts.yaml");
    if path.exists() {
        return Ok(false);
    }
    let file = KnownHostsFile::default();
    known_hosts::write_file(&path, &file).map_err(|e| e.to_string())?;
    Ok(true)
}

fn seed_settings(vet_dir: &std::path::Path) -> Result<bool, String> {
    let path = vet_dir.join("settings.yaml");
    if path.exists() {
        return Ok(false);
    }
    let s = Settings::default();
    settings::write_to(&path, &s).map_err(|e| e.to_string())?;
    Ok(true)
}
