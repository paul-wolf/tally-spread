//! Where Tally keeps its data, and which machine this is.
//!
//! The home folder (`$TALLY_HOME`, default `~/Tally`) holds everything that is
//! synced between machines:
//!
//! ```text
//! ~/Tally/
//!   library/            global reference data (exchange rates)
//!   projects/<name>/    one folder per project
//! ```
//!
//! The machine id is kept outside the home folder, so each machine has its own.

use std::fs;
use std::path::PathBuf;

pub fn tally_home() -> PathBuf {
    std::env::var_os("TALLY_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join("Tally")))
        .unwrap_or_else(|| PathBuf::from("Tally"))
}

pub fn projects_dir() -> PathBuf {
    tally_home().join("projects")
}

pub fn library_dir() -> PathBuf {
    tally_home().join("library")
}

/// A stable id for this machine, e.g. `macos-3k9x2a`. Created on first use;
/// `$TALLY_MACHINE` overrides it.
pub fn machine_id() -> String {
    if let Ok(id) = std::env::var("TALLY_MACHINE") {
        if !id.trim().is_empty() {
            return id.trim().to_string();
        }
    }
    let dir = dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join("tally");
    let path = dir.join("machine-id");
    if let Ok(id) = fs::read_to_string(&path) {
        let id = id.trim().to_string();
        if !id.is_empty() {
            return id;
        }
    }
    let suffix: String = ulid::Ulid::generate().to_string().to_lowercase().chars().rev().take(6).collect();
    let id = format!("{}-{suffix}", std::env::consts::OS);
    let _ = fs::create_dir_all(&dir);
    let _ = fs::write(&path, &id);
    id
}
