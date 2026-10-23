//! The global reference library: data shared by every project, starting with
//! exchange rates.
//!
//! Layout (inside the Tally home folder, which is synced between machines):
//!
//! ```text
//! library/
//!   fx_rates/<machine>.jsonl   one rate record per line, written only by that machine
//!   functions/<machine>.jsonl  user functions; the latest version of each name wins
//! ```
//!
//! Each machine appends only to its own file, so file-level sync (Syncthing) never
//! sees two machines writing the same file. Reading merges all files.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::formula::{Functions, UserFunction};
use crate::fx::{RateRecord, RateTable};
use crate::{Error, Result};

/// A saved version of a user function (or its deletion).
#[derive(Serialize, Deserialize)]
struct FunctionVersion {
    #[serde(flatten)]
    function: UserFunction,
    saved_at: DateTime<Utc>,
    #[serde(default)]
    deleted: bool,
}

/// A rate record as stored, with when it was fetched.
#[derive(Serialize, Deserialize)]
struct Stored {
    #[serde(flatten)]
    rec: RateRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fetched_at: Option<DateTime<Utc>>,
}

pub struct Library {
    dir: PathBuf,
    machine: String,
}

impl Library {
    pub fn open(dir: impl Into<PathBuf>, machine: impl Into<String>) -> Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(dir.join("fx_rates"))?;
        fs::create_dir_all(dir.join("functions"))?;
        Ok(Self { dir, machine: machine.into() })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn rates_dir(&self) -> PathBuf {
        self.dir.join("fx_rates")
    }

    /// All rates from all machines.
    pub fn rates(&self) -> Result<RateTable> {
        let mut table = RateTable::new();
        for entry in fs::read_dir(self.rates_dir())? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            for line in BufReader::new(fs::File::open(&path)?).lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                // A half-synced last line is skipped rather than failing the load.
                if let Ok(st) = serde_json::from_str::<Stored>(&line) {
                    if let Some(at) = st.fetched_at {
                        table.set_fetched(&st.rec, at);
                    }
                    table.add([st.rec]);
                }
            }
        }
        Ok(table)
    }

    /// Add rates, skipping any already known (same source, pair, date and rate).
    /// Returns how many were new.
    pub fn add_rates(&self, records: &[RateRecord]) -> Result<usize> {
        let existing = self.rates()?;
        let known: HashSet<String> = existing.records().map(key).collect();
        let path = self.rates_dir().join(format!("{}.jsonl", self.machine));
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        let mut added = 0;
        let mut seen = HashSet::new();
        for r in records {
            let k = key(r);
            if known.contains(&k) || !seen.insert(k) {
                continue;
            }
            let st = Stored { rec: r.clone(), fetched_at: Some(Utc::now()) };
            writeln!(file, "{}", serde_json::to_string(&st)?)?;
            added += 1;
        }
        file.flush()?;
        Ok(added)
    }
}

impl Library {
    fn function_versions(&self) -> Result<Vec<FunctionVersion>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(self.dir.join("functions"))? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            for line in BufReader::new(fs::File::open(&path)?).lines() {
                if let Ok(v) = serde_json::from_str::<FunctionVersion>(&line?) {
                    out.push(v);
                }
            }
        }
        out.sort_by_key(|v| v.saved_at);
        Ok(out)
    }

    /// The current user functions (latest version of each), and any that no longer compile.
    pub fn functions(&self) -> Result<(Functions, Vec<String>)> {
        let mut latest: std::collections::BTreeMap<String, FunctionVersion> = Default::default();
        for v in self.function_versions()? {
            latest.insert(v.function.name.clone(), v);
        }
        Ok(Functions::new(latest.into_values().filter(|v| !v.deleted).map(|v| v.function)))
    }

    fn append_function(&self, v: &FunctionVersion) -> Result<()> {
        let path = self.dir.join("functions").join(format!("{}.jsonl", self.machine));
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(file, "{}", serde_json::to_string(v)?)?;
        Ok(())
    }

    /// Save (add or replace) a user function after checking it compiles and does
    /// not call itself round in a loop. `old_name` renames an existing one.
    pub fn save_function(&self, f: UserFunction, old_name: Option<&str>) -> Result<()> {
        let mut f = f;
        f.name = f.name.trim().to_string();
        f.body = f.body.trim().trim_start_matches('=').trim().to_string();
        f.params = f.params.iter().map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect();
        crate::formula::compile_function(&f).map_err(Error::InvalidValue)?;
        let (current, _) = self.functions()?;
        let mut all: Vec<UserFunction> =
            current.list().into_iter().filter(|g| g.name != f.name && Some(g.name.as_str()) != old_name).cloned().collect();
        all.push(f.clone());
        let (check, _) = Functions::new(all);
        if check.has_loop(&f.name) {
            return Err(Error::InvalidValue(format!("{} would end up calling itself", f.name)));
        }
        if let Some(old) = old_name.filter(|o| *o != f.name) {
            self.delete_function(old)?;
        }
        self.append_function(&FunctionVersion { function: f, saved_at: Utc::now(), deleted: false })
    }

    pub fn delete_function(&self, name: &str) -> Result<()> {
        let f = UserFunction { name: name.to_string(), params: vec![], body: String::new(), description: String::new() };
        self.append_function(&FunctionVersion { function: f, saved_at: Utc::now(), deleted: true })
    }
}

fn key(r: &RateRecord) -> String {
    format!("{:?}|{}|{}|{}|{}", r.source, r.base, r.quote, r.date, r.rate.normalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::sources::parse_nbu_json;

    #[test]
    fn user_functions_are_saved_and_shared() {
        let tmp = tempfile::tempdir().unwrap();
        let mac = Library::open(tmp.path(), "mac").unwrap();
        let f = |name: &str, params: &[&str], body: &str| UserFunction {
            name: name.into(),
            params: params.iter().map(|p| p.to_string()).collect(),
            body: body.into(),
            description: String::new(),
        };
        mac.save_function(f("vat", &["x"], "x * 0.2"), None).unwrap();
        mac.save_function(f("vat", &["x"], "x * 0.25"), None).unwrap();
        assert!(mac.save_function(f("bad", &["x"], "x * qty"), None).is_err());
        mac.save_function(f("a", &["x"], "x"), None).unwrap();
        assert!(mac.save_function(f("a", &["x"], "b(x)"), None).is_ok(), "b does not exist yet: fine");
        assert!(mac.save_function(f("b", &["x"], "a(x)"), None).is_err(), "would loop");
        let hub = Library::open(tmp.path(), "hub").unwrap();
        let (fns, _) = hub.functions().unwrap();
        assert_eq!(fns.get("vat").unwrap().0.body, "x * 0.25");
        hub.delete_function("vat").unwrap();
        assert!(mac.functions().unwrap().0.get("vat").is_none());
    }

    #[test]
    fn stores_and_merges_rates_from_machines() {
        let tmp = tempfile::tempdir().unwrap();
        let json = r#"[{"cc":"USD","rate":41.7452,"exchangedate":"22.10.2026"}]"#;
        let recs = parse_nbu_json(json).unwrap();

        let mac = Library::open(tmp.path(), "mac").unwrap();
        assert_eq!(mac.add_rates(&recs).unwrap(), 1);
        assert_eq!(mac.add_rates(&recs).unwrap(), 0, "duplicates are skipped");

        let hub = Library::open(tmp.path(), "hub").unwrap();
        assert_eq!(hub.rates().unwrap().len(), 1);
        assert_eq!(hub.add_rates(&recs).unwrap(), 0, "known from the other machine");
    }
}
