//! Where Tally keeps its data, and which machine this is.
//!
//! The home folder (`$TALLY_HOME`, default `~/Tally`) holds everything that is
//! synced between machines:
//!
//! ```text
//! ~/Tally/
//!   library/            global reference data (exchange rates)
//!   projects/<name>/    one folder per project
//!   projects/_unfiled/  sheets not filed in a project yet
//!   .open/<machine>/    tabs the command line asked this machine's app to open
//! ```
//!
//! The machine id is kept outside the home folder, so each machine has its own.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Currency, Project, ProjectSettings, Result};

/// Folder name of the hidden project that holds sheets not filed in any project.
pub const UNFILED: &str = "_unfiled";

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

/// Open a project by folder name under `projects`, creating Unfiled the first time
/// it is needed: USD records, no tax home (spending lists are not tax work).
pub fn open_project_dir(projects: &Path, dir_name: &str, machine: &str) -> Result<Project> {
    let dir = projects.join(dir_name);
    if dir_name == UNFILED && !dir.join("project.json").exists() {
        let mut s = ProjectSettings::new("Unfiled", Currency::new("USD")?);
        s.tax_home = None;
        return Project::create(&dir, s, machine);
    }
    Project::open(dir, machine)
}

/// Sheets the command line asks the app to open as tabs, showing the first.
/// Requests are files in `<home>/.open/<machine>/`, so one made on the hub never
/// opens on the Mac when the home folder syncs. The app picks them up when it
/// starts and every few seconds while it runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OpenRequest {
    /// Project folder name (`_unfiled` for Unfiled).
    pub project: String,
    pub sheet_ids: Vec<String>,
}

fn open_requests_dir(home: &Path, machine: &str) -> PathBuf {
    home.join(".open").join(machine)
}

/// Requests older than this are dropped when a new one is made, so they don't pile
/// up on a machine where the app never runs (the hub).
const OPEN_REQUEST_DAYS: u64 = 7;

/// Leave a request for this machine's app. Written to a temporary name and renamed,
/// so the app never reads half a request.
pub fn request_open(home: &Path, machine: &str, req: &OpenRequest) -> Result<()> {
    let dir = open_requests_dir(home, machine);
    fs::create_dir_all(&dir)?;
    let max_age = std::time::Duration::from_secs(OPEN_REQUEST_DAYS * 24 * 3600);
    for e in fs::read_dir(&dir)?.flatten() {
        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > max_age);
        if old {
            let _ = fs::remove_file(e.path());
        }
    }
    // Named so they sort in the order they were made (a ULID is random within a
    // millisecond): time in nanoseconds, then a counter, then the process.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = format!("{nanos:020}-{n:06}-{}", std::process::id());
    let tmp = dir.join(format!("{name}.tmp"));
    fs::write(&tmp, serde_json::to_vec(req)?)?;
    fs::rename(&tmp, dir.join(format!("{name}.json")))?;
    Ok(())
}

/// Take this machine's requests, oldest first, removing them. Unreadable ones are
/// dropped.
pub fn take_open_requests(home: &Path, machine: &str) -> Vec<OpenRequest> {
    let Ok(entries) = fs::read_dir(open_requests_dir(home, machine)) else { return vec![] };
    let mut paths: Vec<PathBuf> =
        entries.flatten().map(|e| e.path()).filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json")).collect();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|p| {
            let req = fs::read(&p).ok().and_then(|b| serde_json::from_slice(&b).ok());
            let _ = fs::remove_file(&p);
            req
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_requests_are_per_machine_and_taken_once_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let a = OpenRequest { project: UNFILED.into(), sheet_ids: vec!["s1".into()] };
        let b = OpenRequest { project: "flat".into(), sheet_ids: vec!["s2".into(), "s3".into()] };
        request_open(tmp.path(), "mac", &a).unwrap();
        request_open(tmp.path(), "mac", &b).unwrap();
        request_open(tmp.path(), "hub", &a).unwrap();
        assert_eq!(take_open_requests(tmp.path(), "mac"), vec![a.clone(), b]);
        assert!(take_open_requests(tmp.path(), "mac").is_empty());
        assert_eq!(take_open_requests(tmp.path(), "hub"), vec![a]);
    }

    #[test]
    fn unfiled_is_created_on_first_use() {
        let tmp = tempfile::tempdir().unwrap();
        let p = open_project_dir(tmp.path(), UNFILED, "mac").unwrap();
        assert_eq!(p.settings().name, "Unfiled");
        assert_eq!(p.settings().reporting_currency.code(), "USD");
        assert!(p.settings().tax_home.is_none());
        assert!(open_project_dir(tmp.path(), "missing", "mac").is_err());
    }
}
