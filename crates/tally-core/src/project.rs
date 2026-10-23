//! A project: one area of work (e.g. "Drone build") with its own sheets and
//! settings. On disk it is a folder:
//!
//! ```text
//! <project>/
//!   project.json         settings
//!   ops/<machine>.jsonl  append-only operation log, one file per machine
//! ```
//!
//! The current state lives in an in-memory DuckDB database rebuilt by replaying
//! the logs, so the logs are the only thing that needs syncing or backing up.
//! Each machine writes only its own log file, so file sync never has two machines
//! writing the same file.
//!
//! Sheets have user-defined columns (see [`ColumnKind`]); values are stored per
//! cell, normalised for the column's type.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use chrono::Utc;
use duckdb::{params, Connection, OptionalExt};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::formula;
use crate::fx::RateSource;
use crate::money::{parse_amount, Currency, Rounding, RoundingScope};
use crate::ops::{ColumnKind, ColumnSpec, FixedRate, LogEntry, Op, RateMode, RowKind};
use crate::{Error, Result};

/// Version of the project format. Projects made by an older version cannot be opened.
pub const FORMAT: u32 = 2;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ProjectSettings {
    #[serde(default)]
    pub format: u32,
    pub name: String,
    /// The currency the records are kept in (e.g. GBP for UK tax).
    pub reporting_currency: Currency,
    /// Rate sources in order of preference; a sheet can choose its own.
    pub rate_sources: Vec<RateSource>,
    #[serde(default)]
    pub rounding: Rounding,
    #[serde(default)]
    pub rounding_scope: RoundingScope,
    /// Country whose tax rules apply (ISO 3166 code, e.g. "GB").
    #[serde(default)]
    pub tax_home: Option<String>,
    /// Short notes about the project, given to the AI as context.
    #[serde(default)]
    pub notes: String,
}

impl ProjectSettings {
    pub fn new(name: &str, reporting: Currency) -> Self {
        Self {
            format: FORMAT,
            name: name.to_string(),
            reporting_currency: reporting,
            rate_sources: vec![RateSource::Ecb, RateSource::Nbu, RateSource::HmrcMonthly, RateSource::Manual],
            rounding: Rounding::default(),
            rounding_scope: RoundingScope::default(),
            tax_home: None,
            notes: String::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct SheetInfo {
    pub sheet_id: String,
    pub name: String,
    pub archived: bool,
    pub rates: RateMode,
    /// Rate sources chosen for this sheet (empty = the project's).
    pub sources: Vec<RateSource>,
    pub fixed_rates: Vec<FixedRate>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ColumnInfo {
    pub col_id: String,
    pub position: f64,
    #[serde(flatten)]
    pub spec: ColumnSpec,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct RowInfo {
    pub row_id: String,
    pub position: f64,
    pub kind: RowKind,
}

const SCHEMA: &str = "
CREATE TABLE sheets (
    sheet_id VARCHAR NOT NULL,
    name VARCHAR NOT NULL,
    position INTEGER NOT NULL,
    archived BOOLEAN NOT NULL DEFAULT false,
    rates VARCHAR NOT NULL DEFAULT 'live',
    sources VARCHAR NOT NULL DEFAULT '[]',
    fixed_rates VARCHAR NOT NULL DEFAULT '[]'
);
CREATE TABLE columns (
    sheet_id VARCHAR NOT NULL,
    col_id VARCHAR NOT NULL,
    position DOUBLE NOT NULL,
    name VARCHAR NOT NULL,
    spec VARCHAR NOT NULL,
    deleted BOOLEAN NOT NULL DEFAULT false
);
CREATE TABLE sheet_rows (
    sheet_id VARCHAR NOT NULL,
    row_id VARCHAR NOT NULL,
    position DOUBLE NOT NULL,
    kind VARCHAR NOT NULL DEFAULT 'item',
    deleted BOOLEAN NOT NULL DEFAULT false
);
CREATE TABLE cells (
    sheet_id VARCHAR NOT NULL,
    row_id VARCHAR NOT NULL,
    col_id VARCHAR NOT NULL,
    value VARCHAR NOT NULL
);
";

pub struct Project {
    dir: PathBuf,
    machine: String,
    settings: ProjectSettings,
    conn: Connection,
    last_id: Ulid,
    entries_applied: usize,
    undo: Vec<Op>,
    redo: Vec<Op>,
    /// Problems met while replaying logs (e.g. an edit to a row another machine deleted).
    replay_warnings: Vec<String>,
}

/// Read a project's settings without opening it.
pub fn read_settings(dir: &Path) -> Result<ProjectSettings> {
    Ok(serde_json::from_str(&fs::read_to_string(dir.join("project.json"))?)?)
}

impl Project {
    /// Create a new project folder.
    pub fn create(dir: impl Into<PathBuf>, mut settings: ProjectSettings, machine: &str) -> Result<Self> {
        let dir = dir.into();
        if dir.join("project.json").exists() {
            return Err(Error::InvalidValue(format!("a project already exists in {}", dir.display())));
        }
        settings.format = FORMAT;
        fs::create_dir_all(dir.join("ops"))?;
        fs::write(dir.join("project.json"), serde_json::to_string_pretty(&settings)?)?;
        Self::open(dir, machine)
    }

    /// Open a project folder and replay its logs.
    pub fn open(dir: impl Into<PathBuf>, machine: &str) -> Result<Self> {
        let dir = dir.into();
        let settings = read_settings(&dir)?;
        if settings.format < FORMAT {
            return Err(Error::InvalidValue(format!(
                "{} was made by an earlier version of Tally and cannot be opened",
                dir.display()
            )));
        }
        fs::create_dir_all(dir.join("ops"))?;
        let mut p = Self {
            dir,
            machine: machine.to_string(),
            settings,
            conn: Connection::open_in_memory()?,
            last_id: Ulid::nil(),
            entries_applied: 0,
            undo: vec![],
            redo: vec![],
            replay_warnings: vec![],
        };
        p.rebuild()?;
        Ok(p)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn settings(&self) -> &ProjectSettings {
        &self.settings
    }

    pub fn save_settings(&mut self, mut settings: ProjectSettings) -> Result<()> {
        settings.format = FORMAT;
        fs::write(self.dir.join("project.json"), serde_json::to_string_pretty(&settings)?)?;
        self.settings = settings;
        Ok(())
    }

    /// The project's DuckDB connection (tables: sheets, columns, sheet_rows, cells).
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    pub fn replay_warnings(&self) -> &[String] {
        &self.replay_warnings
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    // ---------------------------------------------------------------- logs

    fn log_path(&self) -> PathBuf {
        self.dir.join("ops").join(format!("{}.jsonl", self.machine))
    }

    /// All log entries in replay order, plus a note for each line that could not be read.
    fn read_log(&self) -> Result<(Vec<LogEntry>, Vec<String>)> {
        let mut entries = Vec::new();
        let mut unreadable = Vec::new();
        for entry in fs::read_dir(self.dir.join("ops"))? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            for line in BufReader::new(fs::File::open(&path)?).lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                // A partly synced last line is skipped; it is read on the next refresh.
                match serde_json::from_str::<LogEntry>(&line) {
                    Ok(e) => entries.push(e),
                    Err(err) => unreadable.push(format!("unreadable line in {}: {err}", path.display())),
                }
            }
        }
        entries.sort_by(|a, b| a.id.cmp(&b.id).then_with(|| a.machine.cmp(&b.machine)));
        Ok((entries, unreadable))
    }

    fn append_log(&mut self, op: &Op) -> Result<()> {
        let mut id = Ulid::generate();
        if id <= self.last_id {
            id = self.last_id.increment().unwrap_or(id);
        }
        self.last_id = id;
        let entry = LogEntry { id, ts: Utc::now(), machine: self.machine.clone(), op: op.clone() };
        let mut f = OpenOptions::new().create(true).append(true).open(self.log_path())?;
        writeln!(f, "{}", serde_json::to_string(&entry)?)?;
        f.flush()?;
        self.entries_applied += 1;
        Ok(())
    }

    fn rebuild(&mut self) -> Result<()> {
        self.conn.execute_batch(
            "DROP TABLE IF EXISTS sheets; DROP TABLE IF EXISTS columns;
             DROP TABLE IF EXISTS sheet_rows; DROP TABLE IF EXISTS cells;",
        )?;
        self.conn.execute_batch(SCHEMA)?;
        let (entries, unreadable) = self.read_log()?;
        self.replay_warnings = unreadable;
        for e in &entries {
            self.conn.execute_batch("BEGIN TRANSACTION")?;
            match self.exec(&e.op) {
                Ok(_) => self.conn.execute_batch("COMMIT")?,
                Err(err) => {
                    self.conn.execute_batch("ROLLBACK")?;
                    self.replay_warnings.push(format!("skipped change {} from {}: {err}", e.id, e.machine));
                }
            }
            if e.id > self.last_id {
                self.last_id = e.id;
            }
        }
        self.entries_applied = entries.len();
        Ok(())
    }

    /// Pick up changes synced from other machines. Returns true if anything changed.
    pub fn refresh(&mut self) -> Result<bool> {
        let n = self.read_log()?.0.len();
        if n == self.entries_applied {
            return Ok(false);
        }
        self.rebuild()?;
        Ok(true)
    }

    // ---------------------------------------------------------------- applying

    /// Apply an operation: validate and normalise it, change the state, log it and
    /// remember how to undo it. Nothing changes if it fails.
    pub fn apply(&mut self, op: Op) -> Result<()> {
        let inverse = self.apply_logged(op)?;
        self.undo.push(inverse);
        self.redo.clear();
        Ok(())
    }

    pub fn undo(&mut self) -> Result<bool> {
        let Some(op) = self.undo.pop() else { return Ok(false) };
        let inverse = self.apply_logged(op)?;
        self.redo.push(inverse);
        Ok(true)
    }

    pub fn redo(&mut self) -> Result<bool> {
        let Some(op) = self.redo.pop() else { return Ok(false) };
        let inverse = self.apply_logged(op)?;
        self.undo.push(inverse);
        Ok(true)
    }

    fn apply_logged(&mut self, op: Op) -> Result<Op> {
        self.conn.execute_batch("BEGIN TRANSACTION")?;
        match self.exec(&op) {
            Ok((normalized, inverse)) => {
                self.conn.execute_batch("COMMIT")?;
                self.append_log(&normalized)?;
                Ok(inverse)
            }
            Err(e) => {
                self.conn.execute_batch("ROLLBACK")?;
                Err(e)
            }
        }
    }

    /// Execute one operation against the database. Returns the normalised
    /// operation (what gets logged) and its inverse (what undoes it).
    fn exec(&self, op: &Op) -> Result<(Op, Op)> {
        match op {
            Op::CreateSheet { sheet_id, name } => {
                let name = non_empty(name, "sheet name")?;
                if self.sheet_exists(sheet_id)? {
                    return Err(Error::InvalidValue(format!("sheet {sheet_id} already exists")));
                }
                let position: i64 = self.conn.query_row("SELECT count(*) FROM sheets", [], |r| r.get(0))?;
                self.conn.execute(
                    "INSERT INTO sheets (sheet_id, name, position) VALUES (?, ?, ?)",
                    params![sheet_id, name, position],
                )?;
                Ok((
                    Op::CreateSheet { sheet_id: sheet_id.clone(), name },
                    Op::SetSheetArchived { sheet_id: sheet_id.clone(), archived: true },
                ))
            }
            Op::RenameSheet { sheet_id, name } => {
                let name = non_empty(name, "sheet name")?;
                let old = self.sheet(sheet_id)?.name;
                self.conn.execute("UPDATE sheets SET name = ? WHERE sheet_id = ?", params![name, sheet_id])?;
                Ok((
                    Op::RenameSheet { sheet_id: sheet_id.clone(), name },
                    Op::RenameSheet { sheet_id: sheet_id.clone(), name: old },
                ))
            }
            Op::SetSheetArchived { sheet_id, archived } => {
                let old = self.sheet(sheet_id)?.archived;
                self.conn
                    .execute("UPDATE sheets SET archived = ? WHERE sheet_id = ?", params![archived, sheet_id])?;
                Ok((op.clone(), Op::SetSheetArchived { sheet_id: sheet_id.clone(), archived: old }))
            }
            Op::SetSheetRates { sheet_id, rates } => {
                let old = self.sheet(sheet_id)?.rates;
                self.conn
                    .execute("UPDATE sheets SET rates = ? WHERE sheet_id = ?", params![rates.to_db(), sheet_id])?;
                Ok((op.clone(), Op::SetSheetRates { sheet_id: sheet_id.clone(), rates: old }))
            }
            Op::SetSheetSources { sheet_id, sources } => {
                let old = self.sheet(sheet_id)?.sources;
                let mut unique = Vec::new();
                for s in sources {
                    if !unique.contains(s) {
                        unique.push(*s);
                    }
                }
                self.conn.execute(
                    "UPDATE sheets SET sources = ? WHERE sheet_id = ?",
                    params![serde_json::to_string(&unique)?, sheet_id],
                )?;
                Ok((
                    Op::SetSheetSources { sheet_id: sheet_id.clone(), sources: unique },
                    Op::SetSheetSources { sheet_id: sheet_id.clone(), sources: old },
                ))
            }
            Op::SetSheetFixedRates { sheet_id, rates } => {
                let old = self.sheet(sheet_id)?.fixed_rates;
                let mut clean = Vec::new();
                for r in rates {
                    let (rate, _) = parse_amount(&r.rate)?;
                    if rate <= Decimal::ZERO || r.base == r.quote {
                        return Err(Error::InvalidValue(format!("1 {} = {} {} is not a usable rate", r.base, r.rate, r.quote)));
                    }
                    clean.push(FixedRate { base: r.base.clone(), quote: r.quote.clone(), rate: rate.to_string() });
                }
                self.conn.execute(
                    "UPDATE sheets SET fixed_rates = ? WHERE sheet_id = ?",
                    params![serde_json::to_string(&clean)?, sheet_id],
                )?;
                Ok((
                    Op::SetSheetFixedRates { sheet_id: sheet_id.clone(), rates: clean },
                    Op::SetSheetFixedRates { sheet_id: sheet_id.clone(), rates: old },
                ))
            }

            Op::AddColumn { sheet_id, col_id, position, spec } => {
                self.sheet(sheet_id)?;
                if self.column_row(sheet_id, col_id)?.is_some() {
                    return Err(Error::InvalidValue(format!("column {col_id} already exists")));
                }
                finite(*position)?;
                let spec = self.check_spec(sheet_id, None, spec)?;
                self.conn.execute(
                    "INSERT INTO columns (sheet_id, col_id, position, name, spec) VALUES (?, ?, ?, ?, ?)",
                    params![sheet_id, col_id, position, spec.name, serde_json::to_string(&spec)?],
                )?;
                Ok((
                    Op::AddColumn { sheet_id: sheet_id.clone(), col_id: col_id.clone(), position: *position, spec },
                    Op::SetColumnDeleted { sheet_id: sheet_id.clone(), col_id: col_id.clone(), deleted: true },
                ))
            }
            Op::UpdateColumn { sheet_id, col_id, spec } => {
                let old = self.column(sheet_id, col_id)?;
                let spec = self.check_spec(sheet_id, Some(col_id), spec)?;
                self.write_spec(sheet_id, col_id, &spec)?;
                let mut done = vec![Op::UpdateColumn { sheet_id: sheet_id.clone(), col_id: col_id.clone(), spec: spec.clone() }];
                let mut undo = vec![Op::UpdateColumn { sheet_id: sheet_id.clone(), col_id: col_id.clone(), spec: old.spec.clone() }];

                // Keep other columns' formulas pointing at this column after a rename.
                if old.spec.name != spec.name {
                    for other in self.columns(sheet_id)? {
                        if other.col_id == *col_id {
                            continue;
                        }
                        if let ColumnKind::Calc { formula: src } = &other.spec.kind {
                            let Ok(expr) = formula::parse(src) else { continue };
                            if !formula::names(&expr).iter().any(|n| n.eq_ignore_ascii_case(&old.spec.name)) {
                                continue;
                            }
                            let new_src = formula::rename(&expr, &old.spec.name, &spec.name).to_string();
                            let new_spec = ColumnSpec { kind: ColumnKind::Calc { formula: new_src }, ..other.spec.clone() };
                            self.write_spec(sheet_id, &other.col_id, &new_spec)?;
                            done.push(Op::UpdateColumn { sheet_id: sheet_id.clone(), col_id: other.col_id.clone(), spec: new_spec });
                            undo.push(Op::UpdateColumn { sheet_id: sheet_id.clone(), col_id: other.col_id.clone(), spec: other.spec.clone() });
                        }
                    }
                }
                undo.reverse();
                if done.len() == 1 {
                    Ok((done.pop().unwrap(), undo.pop().unwrap()))
                } else {
                    Ok((Op::Batch { ops: done }, Op::Batch { ops: undo }))
                }
            }
            Op::MoveColumn { sheet_id, col_id, position } => {
                finite(*position)?;
                let old = self.column(sheet_id, col_id)?.position;
                self.conn.execute(
                    "UPDATE columns SET position = ? WHERE sheet_id = ? AND col_id = ?",
                    params![position, sheet_id, col_id],
                )?;
                Ok((op.clone(), Op::MoveColumn { sheet_id: sheet_id.clone(), col_id: col_id.clone(), position: old }))
            }
            Op::SetColumnDeleted { sheet_id, col_id, deleted } => {
                let (_, old) = self
                    .column_row(sheet_id, col_id)?
                    .ok_or_else(|| Error::NotFound(format!("column {col_id}")))?;
                if !deleted && old {
                    // Restoring: its name must still be free.
                    let spec = self.column_spec_any(sheet_id, col_id)?;
                    if self.name_taken(sheet_id, Some(col_id), &spec.name)? {
                        return Err(Error::InvalidValue(format!("another column is already called {}", spec.name)));
                    }
                }
                self.conn.execute(
                    "UPDATE columns SET deleted = ? WHERE sheet_id = ? AND col_id = ?",
                    params![deleted, sheet_id, col_id],
                )?;
                Ok((
                    op.clone(),
                    Op::SetColumnDeleted { sheet_id: sheet_id.clone(), col_id: col_id.clone(), deleted: old },
                ))
            }

            Op::InsertRow { sheet_id, row_id, position } => {
                self.sheet(sheet_id)?;
                if self.row_state(sheet_id, row_id)?.is_some() {
                    return Err(Error::InvalidValue(format!("row {row_id} already exists")));
                }
                finite(*position)?;
                self.conn.execute(
                    "INSERT INTO sheet_rows (sheet_id, row_id, position) VALUES (?, ?, ?)",
                    params![sheet_id, row_id, position],
                )?;
                Ok((
                    op.clone(),
                    Op::SetRowDeleted { sheet_id: sheet_id.clone(), row_id: row_id.clone(), deleted: true },
                ))
            }
            Op::SetRowKind { sheet_id, row_id, kind } => {
                let (old, _) = self.row_state(sheet_id, row_id)?.ok_or_else(|| Error::NotFound(format!("row {row_id}")))?;
                self.conn.execute(
                    "UPDATE sheet_rows SET kind = ? WHERE sheet_id = ? AND row_id = ?",
                    params![kind.as_str(), sheet_id, row_id],
                )?;
                Ok((op.clone(), Op::SetRowKind { sheet_id: sheet_id.clone(), row_id: row_id.clone(), kind: old }))
            }
            Op::SetRowDeleted { sheet_id, row_id, deleted } => {
                let (_, old) = self.row_state(sheet_id, row_id)?.ok_or_else(|| Error::NotFound(format!("row {row_id}")))?;
                self.conn.execute(
                    "UPDATE sheet_rows SET deleted = ? WHERE sheet_id = ? AND row_id = ?",
                    params![deleted, sheet_id, row_id],
                )?;
                Ok((
                    op.clone(),
                    Op::SetRowDeleted { sheet_id: sheet_id.clone(), row_id: row_id.clone(), deleted: old },
                ))
            }
            Op::SetCell { sheet_id, row_id, col_id, value } => {
                let (row_kind, _) =
                    self.row_state(sheet_id, row_id)?.ok_or_else(|| Error::NotFound(format!("row {row_id}")))?;
                let col = self.column(sheet_id, col_id)?;
                let old = self.cell(sheet_id, row_id, col_id)?;
                let value = value.as_deref().map(str::trim).filter(|v| !v.is_empty());
                let normalized = match value {
                    None => None,
                    // A section heading is free text in any column.
                    Some(v) if row_kind == RowKind::Section => Some(v.to_string()),
                    Some(v) => Some(normalize_cell(&col.spec, v)?),
                };
                self.conn.execute(
                    "DELETE FROM cells WHERE sheet_id = ? AND row_id = ? AND col_id = ?",
                    params![sheet_id, row_id, col_id],
                )?;
                if let Some(v) = &normalized {
                    self.conn.execute(
                        "INSERT INTO cells (sheet_id, row_id, col_id, value) VALUES (?, ?, ?, ?)",
                        params![sheet_id, row_id, col_id, v],
                    )?;
                }
                let make = |v: Option<String>| Op::SetCell {
                    sheet_id: sheet_id.clone(),
                    row_id: row_id.clone(),
                    col_id: col_id.clone(),
                    value: v,
                };
                Ok((make(normalized), make(old)))
            }
            Op::Batch { ops } => {
                let mut done = Vec::with_capacity(ops.len());
                let mut inverses = Vec::with_capacity(ops.len());
                for o in ops {
                    let (n, i) = self.exec(o)?;
                    done.push(n);
                    inverses.push(i);
                }
                inverses.reverse();
                Ok((Op::Batch { ops: done }, Op::Batch { ops: inverses }))
            }
        }
    }

    /// Validate a column spec; names must be unique (formulas refer to them).
    fn check_spec(&self, sheet_id: &str, col_id: Option<&str>, spec: &ColumnSpec) -> Result<ColumnSpec> {
        let name = non_empty(&spec.name, "column name")?;
        if name.contains(['[', ']']) {
            return Err(Error::InvalidValue("column names cannot contain [ or ]".into()));
        }
        if self.name_taken(sheet_id, col_id, &name)? {
            return Err(Error::InvalidValue(format!("another column is already called {name}")));
        }
        let kind = match &spec.kind {
            ColumnKind::Calc { formula: src } => {
                let expr = formula::parse(src).map_err(|e| Error::InvalidValue(format!("formula: {e}")))?;
                if formula::names(&expr).iter().any(|n| n.eq_ignore_ascii_case(&name)) {
                    return Err(Error::InvalidValue(format!("the formula for {name} refers to itself")));
                }
                ColumnKind::Calc { formula: src.trim().trim_start_matches('=').trim().to_string() }
            }
            other => other.clone(),
        };
        Ok(ColumnSpec { name, kind, total: spec.total })
    }

    fn name_taken(&self, sheet_id: &str, col_id: Option<&str>, name: &str) -> Result<bool> {
        Ok(self
            .columns(sheet_id)?
            .iter()
            .any(|c| Some(c.col_id.as_str()) != col_id && c.spec.name.eq_ignore_ascii_case(name)))
    }

    fn write_spec(&self, sheet_id: &str, col_id: &str, spec: &ColumnSpec) -> Result<()> {
        self.conn.execute(
            "UPDATE columns SET name = ?, spec = ? WHERE sheet_id = ? AND col_id = ?",
            params![spec.name, serde_json::to_string(spec)?, sheet_id, col_id],
        )?;
        Ok(())
    }

    // ---------------------------------------------------------------- reading

    fn sheet_exists(&self, sheet_id: &str) -> Result<bool> {
        let n: i64 =
            self.conn.query_row("SELECT count(*) FROM sheets WHERE sheet_id = ?", params![sheet_id], |r| r.get(0))?;
        Ok(n > 0)
    }

    /// (kind, deleted) of a row, if it exists.
    fn row_state(&self, sheet_id: &str, row_id: &str) -> Result<Option<(RowKind, bool)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT kind, deleted FROM sheet_rows WHERE sheet_id = ? AND row_id = ?",
                params![sheet_id, row_id],
                |r| Ok((RowKind::parse(&r.get::<_, String>(0)?), r.get::<_, bool>(1)?)),
            )
            .optional()?)
    }

    /// (spec json, deleted) of a column, deleted or not.
    fn column_row(&self, sheet_id: &str, col_id: &str) -> Result<Option<(String, bool)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT spec, deleted FROM columns WHERE sheet_id = ? AND col_id = ?",
                params![sheet_id, col_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?)),
            )
            .optional()?)
    }

    fn column_spec_any(&self, sheet_id: &str, col_id: &str) -> Result<ColumnSpec> {
        let (json, _) = self.column_row(sheet_id, col_id)?.ok_or_else(|| Error::NotFound(format!("column {col_id}")))?;
        Ok(serde_json::from_str(&json)?)
    }

    /// A column that is not deleted.
    pub fn column(&self, sheet_id: &str, col_id: &str) -> Result<ColumnInfo> {
        self.columns(sheet_id)?
            .into_iter()
            .find(|c| c.col_id == col_id)
            .ok_or_else(|| Error::NotFound(format!("column {col_id}")))
    }

    fn cell(&self, sheet_id: &str, row_id: &str, col_id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM cells WHERE sheet_id = ? AND row_id = ? AND col_id = ?",
                params![sheet_id, row_id, col_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn sheet(&self, sheet_id: &str) -> Result<SheetInfo> {
        self.sheets(true)?
            .into_iter()
            .find(|s| s.sheet_id == sheet_id)
            .ok_or_else(|| Error::NotFound(format!("sheet {sheet_id}")))
    }

    pub fn sheets(&self, include_archived: bool) -> Result<Vec<SheetInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT sheet_id, name, archived, rates, sources, fixed_rates FROM sheets ORDER BY position, sheet_id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, bool>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (sheet_id, name, archived, rates, sources, fixed) = row?;
            if archived && !include_archived {
                continue;
            }
            out.push(SheetInfo {
                sheet_id,
                name,
                archived,
                rates: RateMode::from_db(&rates)?,
                sources: serde_json::from_str(&sources)?,
                fixed_rates: serde_json::from_str(&fixed)?,
            });
        }
        Ok(out)
    }

    /// Columns of a sheet in order, without deleted ones.
    pub fn columns(&self, sheet_id: &str) -> Result<Vec<ColumnInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT col_id, position, spec FROM columns WHERE sheet_id = ? AND NOT deleted ORDER BY position, col_id",
        )?;
        let rows = stmt.query_map(params![sheet_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?, r.get::<_, String>(2)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (col_id, position, spec) = row?;
            out.push(ColumnInfo { col_id, position, spec: serde_json::from_str(&spec)? });
        }
        Ok(out)
    }

    /// Rows of a sheet in order, without deleted ones.
    pub fn rows(&self, sheet_id: &str) -> Result<Vec<RowInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT row_id, position, kind FROM sheet_rows WHERE sheet_id = ? AND NOT deleted ORDER BY position, row_id",
        )?;
        let rows = stmt.query_map(params![sheet_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?, r.get::<_, String>(2)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (row_id, position, kind) = row?;
            out.push(RowInfo { row_id, position, kind: RowKind::parse(&kind) });
        }
        Ok(out)
    }

    /// All stored values of a sheet: (row_id, col_id) -> value.
    pub fn cells(&self, sheet_id: &str) -> Result<HashMap<(String, String), String>> {
        let mut stmt = self.conn.prepare("SELECT row_id, col_id, value FROM cells WHERE sheet_id = ?")?;
        let rows = stmt.query_map(params![sheet_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (r, c, v) = row?;
            out.insert((r, c), v);
        }
        Ok(out)
    }

    /// A position after the last row of a sheet.
    pub fn next_row_position(&self, sheet_id: &str) -> Result<f64> {
        let max: Option<f64> =
            self.conn.query_row("SELECT max(position) FROM sheet_rows WHERE sheet_id = ?", params![sheet_id], |r| r.get(0))?;
        Ok(max.map_or(1.0, |m| m.floor() + 1.0))
    }

    /// A position after the last column of a sheet.
    pub fn next_column_position(&self, sheet_id: &str) -> Result<f64> {
        let max: Option<f64> =
            self.conn.query_row("SELECT max(position) FROM columns WHERE sheet_id = ?", params![sheet_id], |r| r.get(0))?;
        Ok(max.map_or(1.0, |m| m.floor() + 1.0))
    }
}

/// Normalise a typed or pasted value for a column, or say why it does not fit.
pub fn normalize_cell(spec: &ColumnSpec, v: &str) -> Result<String> {
    match &spec.kind {
        ColumnKind::Text => Ok(v.to_string()),
        ColumnKind::Link => Ok(normalize_url(v)),
        ColumnKind::Number => {
            let (n, cur) = parse_amount(v)?;
            if let Some(c) = cur {
                return Err(Error::InvalidValue(format!("{} holds plain numbers, not {c}", spec.name)));
            }
            Ok(n.to_string())
        }
        ColumnKind::Money { currency } => {
            let (n, cur) = parse_amount(v)?;
            if let Some(c) = cur {
                if &c != currency {
                    return Err(Error::InvalidValue(format!("{} is a {currency} column; {v:?} is in {c}", spec.name)));
                }
            }
            Ok(n.to_string())
        }
        ColumnKind::Calc { .. } => Err(Error::InvalidValue(format!("{} is calculated from a formula", spec.name))),
    }
}

/// `rozetka.com.ua/x` becomes `https://rozetka.com.ua/x`; anything else is kept as typed.
pub fn normalize_url(v: &str) -> String {
    let looks_like_host = v.split('/').next().is_some_and(|h| h.contains('.') && !h.contains(' '));
    if !v.contains("://") && looks_like_host && !v.contains(' ') {
        format!("https://{v}")
    } else {
        v.to_string()
    }
}

fn non_empty(s: &str, what: &str) -> Result<String> {
    let t = s.trim();
    if t.is_empty() {
        Err(Error::InvalidValue(format!("{what} cannot be empty")))
    } else {
        Ok(t.to_string())
    }
}

fn finite(position: f64) -> Result<()> {
    if position.is_finite() {
        Ok(())
    } else {
        Err(Error::InvalidValue("position must be a number".into()))
    }
}


#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::ops::ColumnKind as K;
    use rust_decimal_macros::dec;

    pub fn cur(s: &str) -> Currency {
        Currency::new(s).unwrap()
    }

    pub fn spec(name: &str, kind: K, total: bool) -> ColumnSpec {
        ColumnSpec { name: name.into(), kind, total }
    }

    /// A project with one sheet "s" laid out like a spending list.
    pub fn spending_project(dir: &Path) -> Project {
        let mut p = Project::create(dir, ProjectSettings::new("Test", cur("GBP")), "mac").unwrap();
        let cols = [
            ("item", spec("Item", K::Text, false)),
            ("uah", spec("UAH", K::Money { currency: cur("UAH") }, false)),
            ("usd", spec("USD", K::Money { currency: cur("USD") }, false)),
            ("unit", spec("Unit USD", K::Calc { formula: "coalesce(to(UAH, USD), USD)".into() }, false)),
            ("qty", spec("Qty", K::Number, false)),
            ("total", spec("Total USD", K::Calc { formula: "[Unit USD] * Qty".into() }, true)),
        ];
        let mut ops = vec![Op::CreateSheet { sheet_id: "s".into(), name: "Drone".into() }];
        for (i, (id, s)) in cols.into_iter().enumerate() {
            ops.push(Op::AddColumn { sheet_id: "s".into(), col_id: id.into(), position: i as f64, spec: s });
        }
        p.apply(Op::Batch { ops }).unwrap();
        p
    }

    pub fn add_row(p: &mut Project, row: &str, values: &[(&str, &str)]) {
        let pos = p.next_row_position("s").unwrap();
        let mut ops = vec![Op::InsertRow { sheet_id: "s".into(), row_id: row.into(), position: pos }];
        for (col, v) in values {
            ops.push(Op::SetCell { sheet_id: "s".into(), row_id: row.into(), col_id: col.to_string(), value: Some(v.to_string()) });
        }
        p.apply(Op::Batch { ops }).unwrap();
    }

    #[test]
    fn values_are_normalised_per_column_and_persist() {
        let tmp = tempfile::tempdir().unwrap();
        {
            let mut p = spending_project(tmp.path());
            add_row(&mut p, "r1", &[("item", "Carbon plates"), ("uah", "₴3 250,00"), ("qty", "2")]);
        }
        let p = Project::open(tmp.path(), "mac").unwrap();
        assert!(p.replay_warnings().is_empty(), "{:?}", p.replay_warnings());
        let cells = p.cells("s").unwrap();
        assert_eq!(cells[&("r1".into(), "uah".into())], "3250");
        assert_eq!(cells[&("r1".into(), "qty".into())], "2");
        assert_eq!(p.columns("s").unwrap().len(), 6);
    }

    #[test]
    fn values_that_do_not_fit_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = spending_project(tmp.path());
        add_row(&mut p, "r1", &[]);
        let set = |col: &str, v: &str| Op::SetCell {
            sheet_id: "s".into(),
            row_id: "r1".into(),
            col_id: col.into(),
            value: Some(v.into()),
        };
        assert!(p.apply(set("uah", "$25")).is_err(), "USD in the UAH column");
        assert!(p.apply(set("qty", "lots")).is_err());
        assert!(p.apply(set("unit", "5")).is_err(), "calculated column");
        assert!(p.apply(set("usd", "$25")).is_ok());
    }

    #[test]
    fn columns_insert_rename_delete_and_undo() {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = spending_project(tmp.path());
        // Insert a Link column between Item (0) and UAH (1).
        p.apply(Op::AddColumn { sheet_id: "s".into(), col_id: "link".into(), position: 0.5, spec: spec("Link", K::Link, false) })
            .unwrap();
        let names: Vec<_> = p.columns("s").unwrap().into_iter().map(|c| c.spec.name).collect();
        assert_eq!(names, ["Item", "Link", "UAH", "USD", "Unit USD", "Qty", "Total USD"]);

        // Duplicate names are refused.
        assert!(p
            .apply(Op::AddColumn { sheet_id: "s".into(), col_id: "x".into(), position: 9.0, spec: spec("qty", K::Number, false) })
            .is_err());

        // Renaming Unit USD updates the Total USD formula, and undo restores both.
        p.apply(Op::UpdateColumn {
            sheet_id: "s".into(),
            col_id: "unit".into(),
            spec: spec("Each USD", K::Calc { formula: "coalesce(to(UAH, USD), USD)".into() }, false),
        })
        .unwrap();
        let total = p.column("s", "total").unwrap();
        assert_eq!(total.spec.kind, K::Calc { formula: "[Each USD] * Qty".into() });
        p.undo().unwrap();
        assert_eq!(p.column("s", "total").unwrap().spec.kind, K::Calc { formula: "[Unit USD] * Qty".into() });

        // Delete and restore a column.
        p.apply(Op::SetColumnDeleted { sheet_id: "s".into(), col_id: "link".into(), deleted: true }).unwrap();
        assert_eq!(p.columns("s").unwrap().len(), 6);
        p.undo().unwrap();
        assert_eq!(p.columns("s").unwrap().len(), 7);
    }

    #[test]
    fn bad_formulas_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = spending_project(tmp.path());
        let add = |f: &str| Op::AddColumn {
            sheet_id: "s".into(),
            col_id: "x".into(),
            position: 99.0,
            spec: spec("X", K::Calc { formula: f.into() }, false),
        };
        assert!(p.apply(add("Qty *")).is_err());
        assert!(p.apply(add("X + 1")).is_err(), "refers to itself");
        assert!(p.apply(add("= Qty * 2")).is_ok());
        assert_eq!(p.column("s", "x").unwrap().spec.kind, K::Calc { formula: "Qty * 2".into() });
    }

    #[test]
    fn merges_logs_from_two_machines() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mac = spending_project(tmp.path());
        add_row(&mut mac, "r1", &[("item", "Motor")]);
        let mut hub = Project::open(tmp.path(), "hub").unwrap();
        hub.apply(Op::InsertRow { sheet_id: "s".into(), row_id: "r2".into(), position: 2.0 }).unwrap();
        assert!(mac.refresh().unwrap());
        assert_eq!(mac.rows("s").unwrap().len(), 2);
        assert!(!mac.refresh().unwrap());
        assert!(mac.replay_warnings().is_empty());
    }

    #[test]
    fn old_projects_are_refused_clearly() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("project.json"),
            r#"{"name":"Old","reporting_currency":"GBP","display_currencies":["GBP"],"rate_sources":["ecb"]}"#,
        )
        .unwrap();
        let err = Project::open(tmp.path(), "mac").err().unwrap().to_string();
        assert!(err.contains("earlier version"), "{err}");
    }

    #[test]
    fn urls_get_a_scheme() {
        assert_eq!(normalize_url("rozetka.com.ua/item/1"), "https://rozetka.com.ua/item/1");
        assert_eq!(normalize_url("http://x.com"), "http://x.com");
        assert_eq!(normalize_url("ask Olena"), "ask Olena");
        let _ = dec!(1);
    }
}
