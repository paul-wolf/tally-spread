//! The global reference library: data shared by every project, starting with
//! exchange rates.
//!
//! Layout (inside the Tally home folder, which is synced between machines):
//!
//! ```text
//! library/
//!   fx_rates/<machine>.jsonl   one rate record per line, written only by that machine
//! ```
//!
//! Each machine appends only to its own file, so file-level sync (Syncthing) never
//! sees two machines writing the same file. Reading merges all files.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use crate::fx::{RateRecord, RateTable};
use crate::Result;

pub struct Library {
    dir: PathBuf,
    machine: String,
}

impl Library {
    pub fn open(dir: impl Into<PathBuf>, machine: impl Into<String>) -> Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(dir.join("fx_rates"))?;
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
                if let Ok(rec) = serde_json::from_str::<RateRecord>(&line) {
                    table.add([rec]);
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
            writeln!(file, "{}", serde_json::to_string(r)?)?;
            added += 1;
        }
        file.flush()?;
        Ok(added)
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
