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
use crate::ops::{ColumnKind, ColumnSpec, FixedRate, LogEntry, NewRow, Op, RateMode, RowKind};
use crate::{Error, Result};

/// Version of the project format. Projects made by an older version cannot be opened.
pub const FORMAT: u32 = 3;

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
    /// The project's colour in the app, as a hue in degrees (OKLCH). Chosen when the
    /// project is made so projects look different from each other; `None` for Unfiled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<u16>,
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
            color: None,
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
    /// Rate card base currency chosen for this sheet.
    pub base: Option<Currency>,
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
    fixed_rates VARCHAR NOT NULL DEFAULT '[]',
    base VARCHAR NOT NULL DEFAULT ''
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
    /// User functions from the library, available to formulas.
    functions: formula::Functions,
    /// When each sheet was last changed, from the logs of every machine.
    edited: HashMap<String, chrono::DateTime<Utc>>,
}

/// Read a project's settings without opening it.
pub fn read_settings(dir: &Path) -> Result<ProjectSettings> {
    Ok(serde_json::from_str(&fs::read_to_string(dir.join("project.json"))?)?)
}

impl Project {
    /// Run a statement, prepared once and cached: replaying a log runs the same
    /// few statements thousands of times, and parsing them each time dominates.
    fn run(&self, sql: &str, params: impl duckdb::Params) -> duckdb::Result<usize> {
        self.conn.prepare_cached(sql)?.execute(params)
    }

    fn qrow<T>(&self, sql: &str, params: impl duckdb::Params, f: impl FnOnce(&duckdb::Row<'_>) -> duckdb::Result<T>) -> duckdb::Result<T> {
        self.conn.prepare_cached(sql)?.query_row(params, f)
    }

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
        if settings.format == 2 {
            return Self::migrate_v2(dir, settings, machine);
        }
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
            functions: Default::default(),
            edited: HashMap::new(),
        };
        p.rebuild()?;
        Ok(p)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// When a sheet was last changed on any machine (`None` if never logged).
    pub fn edited(&self, sheet_id: &str) -> Option<chrono::DateTime<Utc>> {
        self.edited.get(sheet_id).copied()
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

    /// Make the library's user functions available to this project's formulas.
    pub fn set_functions(&mut self, functions: formula::Functions) {
        self.functions = functions;
    }

    pub fn functions(&self) -> &formula::Functions {
        &self.functions
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
        for s in op.sheet_ids() {
            self.edited.insert(s.to_string(), entry.ts);
        }
        // One write per entry: the app and the `tally` command on the same machine
        // append to the same log, and an appended write is never interleaved.
        let mut line = serde_json::to_string(&entry)?;
        line.push('\n');
        let mut f = OpenOptions::new().create(true).append(true).open(self.log_path())?;
        f.write_all(line.as_bytes())?;
        self.entries_applied += 1;
        Ok(())
    }

    fn rebuild(&mut self) -> Result<()> {
        // Cached statements point at the tables about to be replaced.
        self.conn.flush_prepared_statement_cache();
        self.conn.execute_batch(
            "DROP TABLE IF EXISTS sheets; DROP TABLE IF EXISTS columns;
             DROP TABLE IF EXISTS sheet_rows; DROP TABLE IF EXISTS cells;",
        )?;
        self.conn.execute_batch(SCHEMA)?;
        self.conn.set_prepared_statement_cache_capacity(64);
        let (entries, unreadable) = self.read_log()?;
        self.replay_warnings = unreadable;
        self.edited.clear();
        for e in &entries {
            self.conn.execute_batch("BEGIN TRANSACTION")?;
            match self.exec(&e.op) {
                Ok(_) => {
                    self.conn.execute_batch("COMMIT")?;
                    for s in e.op.sheet_ids() {
                        self.edited.insert(s.to_string(), e.ts);
                    }
                }
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
                let position: i64 = self.qrow("SELECT count(*) FROM sheets", [], |r| r.get(0))?;
                self.run(
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
                self.run("UPDATE sheets SET name = ? WHERE sheet_id = ?", params![name, sheet_id])?;
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
                self.run(
                    "UPDATE sheets SET sources = ? WHERE sheet_id = ?",
                    params![serde_json::to_string(&unique)?, sheet_id],
                )?;
                Ok((
                    Op::SetSheetSources { sheet_id: sheet_id.clone(), sources: unique },
                    Op::SetSheetSources { sheet_id: sheet_id.clone(), sources: old },
                ))
            }
            Op::SetSheetBase { sheet_id, base } => {
                let old = self.sheet(sheet_id)?.base;
                let text = base.as_ref().map(|c| c.code().to_string()).unwrap_or_default();
                self.run("UPDATE sheets SET base = ? WHERE sheet_id = ?", params![text, sheet_id])?;
                Ok((op.clone(), Op::SetSheetBase { sheet_id: sheet_id.clone(), base: old }))
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
                self.run(
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
                let col_id = if col_id.is_empty() { self.next_col_id(sheet_id)? } else { col_id.clone() };
                if self.column_row(sheet_id, &col_id)?.is_some() {
                    return Err(Error::InvalidValue(format!("column {col_id} already exists")));
                }
                finite(*position)?;
                let spec = self.check_spec(sheet_id, &col_id, spec)?;
                self.run(
                    "INSERT INTO columns (sheet_id, col_id, position, name, spec) VALUES (?, ?, ?, ?, ?)",
                    params![sheet_id, col_id, position, spec.name, serde_json::to_string(&spec)?],
                )?;
                Ok((
                    Op::AddColumn { sheet_id: sheet_id.clone(), col_id: col_id.clone(), position: *position, spec },
                    Op::SetColumnDeleted { sheet_id: sheet_id.clone(), col_id, deleted: true },
                ))
            }
            Op::UpdateColumn { sheet_id, col_id, spec } => {
                let old = self.column(sheet_id, col_id)?;
                let spec = self.check_spec(sheet_id, col_id, spec)?;
                self.write_spec(sheet_id, col_id, &spec)?;
                let update = Op::UpdateColumn { sheet_id: sheet_id.clone(), col_id: col_id.clone(), spec: spec.clone() };
                let restore = Op::UpdateColumn { sheet_id: sheet_id.clone(), col_id: col_id.clone(), spec: old.spec.clone() };
                if old.spec.kind == spec.kind || !spec.kind.is_input() {
                    return Ok((update, restore));
                }
                // The type changed: convert values that fit the new type ("UAH 3,250.00"
                // -> 3250 in a UAH column); leave the rest as typed, shown as problems.
                let items: std::collections::HashSet<String> =
                    self.rows(sheet_id)?.into_iter().filter(|r| r.kind == RowKind::Item).map(|r| r.row_id).collect();
                let mut values = Vec::new();
                for (row_id, raw) in self.column_cells(sheet_id, col_id)? {
                    if !items.contains(&row_id) {
                        continue;
                    }
                    let Ok(norm) = normalize_cell(&spec, &raw) else { continue };
                    if norm != raw {
                        values.push((row_id, Some(norm)));
                    }
                }
                if values.is_empty() {
                    return Ok((update, restore));
                }
                let (set, unset) = self.exec(&Op::SetCells { sheet_id: sheet_id.clone(), col_id: col_id.clone(), values })?;
                // Undo restores the old type first, so the old text goes back unconverted.
                Ok((Op::Batch { ops: vec![update, set] }, Op::Batch { ops: vec![restore, unset] }))
            }
            Op::MoveColumn { sheet_id, col_id, position } => {
                finite(*position)?;
                let old = self.column(sheet_id, col_id)?.position;
                self.run(
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
                    // Restoring: its names must still be free.
                    let spec = self.column_spec_any(sheet_id, col_id)?;
                    if let Some(clash) = self.columns(sheet_id)?.iter().find(|c| {
                        c.spec.name.eq_ignore_ascii_case(&spec.name) || (!spec.var.is_empty() && c.spec.var == spec.var)
                    }) {
                        return Err(Error::InvalidValue(format!("column {} already uses that name", clash.col_id)));
                    }
                }
                self.run(
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
                self.run(
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
                self.run(
                    "UPDATE sheet_rows SET kind = ? WHERE sheet_id = ? AND row_id = ?",
                    params![kind.as_str(), sheet_id, row_id],
                )?;
                Ok((op.clone(), Op::SetRowKind { sheet_id: sheet_id.clone(), row_id: row_id.clone(), kind: old }))
            }
            Op::SetRowDeleted { sheet_id, row_id, deleted } => {
                let (_, old) = self.row_state(sheet_id, row_id)?.ok_or_else(|| Error::NotFound(format!("row {row_id}")))?;
                self.run(
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
                self.run(
                    "DELETE FROM cells WHERE sheet_id = ? AND row_id = ? AND col_id = ?",
                    params![sheet_id, row_id, col_id],
                )?;
                if let Some(v) = &normalized {
                    self.run(
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
            Op::SetCells { sheet_id, col_id, values } => {
                self.column(sheet_id, col_id)?;
                // Read the column, change it in memory, write it back in one go.
                let mut cells: HashMap<String, String> = self.column_cells(sheet_id, col_id)?.into_iter().collect();
                let mut old = Vec::with_capacity(values.len());
                for (row_id, v) in values {
                    old.push((row_id.clone(), cells.get(row_id).cloned()));
                    match v {
                        Some(v) => cells.insert(row_id.clone(), v.clone()),
                        None => cells.remove(row_id),
                    };
                }
                self.run("DELETE FROM cells WHERE sheet_id = ? AND col_id = ?", params![sheet_id, col_id])?;
                {
                    let mut app = self.conn.appender("cells")?;
                    for (row_id, v) in &cells {
                        app.append_row(params![sheet_id, row_id, col_id, v])?;
                    }
                }
                Ok((op.clone(), Op::SetCells { sheet_id: sheet_id.clone(), col_id: col_id.clone(), values: old }))
            }
            Op::InsertRows { sheet_id, rows } => {
                self.sheet(sheet_id)?;
                let columns: HashMap<String, ColumnSpec> =
                    self.columns(sheet_id)?.into_iter().map(|c| (c.col_id, c.spec)).collect();
                let mut existing: std::collections::HashSet<String> = std::collections::HashSet::new();
                {
                    let mut st = self.conn.prepare_cached("SELECT row_id FROM sheet_rows WHERE sheet_id = ?")?;
                    let ids = st.query_map(params![sheet_id], |r| r.get::<_, String>(0))?;
                    for id in ids {
                        existing.insert(id?);
                    }
                }
                // Check and normalise everything before writing anything.
                let mut clean: Vec<NewRow> = Vec::with_capacity(rows.len());
                for (n, r) in rows.iter().enumerate() {
                    finite(r.position)?;
                    if !existing.insert(r.row_id.clone()) {
                        return Err(Error::InvalidValue(format!("row {} already exists", r.row_id)));
                    }
                    let mut cells = std::collections::BTreeMap::new();
                    for (col_id, v) in &r.cells {
                        let spec = columns.get(col_id).ok_or_else(|| Error::NotFound(format!("column {col_id}")))?;
                        let v = v.trim();
                        if v.is_empty() {
                            continue;
                        }
                        let norm = normalize_cell(spec, v)
                            .map_err(|e| Error::InvalidValue(format!("row {}, {}: {e}", n + 1, spec.name)))?;
                        cells.insert(col_id.clone(), norm);
                    }
                    clean.push(NewRow { row_id: r.row_id.clone(), position: r.position, cells });
                }
                {
                    let mut app = self.conn.appender("sheet_rows")?;
                    for r in &clean {
                        app.append_row(params![sheet_id, r.row_id, r.position, "item", false])?;
                    }
                }
                {
                    let mut app = self.conn.appender("cells")?;
                    for r in &clean {
                        for (col_id, v) in &r.cells {
                            app.append_row(params![sheet_id, r.row_id, col_id, v])?;
                        }
                    }
                }
                let undo = clean
                    .iter()
                    .map(|r| Op::SetRowDeleted { sheet_id: sheet_id.clone(), row_id: r.row_id.clone(), deleted: true })
                    .collect();
                Ok((Op::InsertRows { sheet_id: sheet_id.clone(), rows: clean }, Op::Batch { ops: undo }))
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

    /// The next column ID for a sheet: `c` + one more than any ID it ever had.
    fn next_col_id(&self, sheet_id: &str) -> Result<String> {
        let mut stmt = self.conn.prepare_cached("SELECT col_id FROM columns WHERE sheet_id = ?")?;
        let ids = stmt.query_map(params![sheet_id], |r| r.get::<_, String>(0))?;
        let mut max = 0u32;
        for id in ids {
            if let Some(n) = id?.strip_prefix('c').and_then(|n| n.parse::<u32>().ok()) {
                max = max.max(n);
            }
        }
        Ok(format!("c{}", max + 1))
    }

    /// How formulas see the sheet's columns.
    pub fn col_refs(&self, sheet_id: &str) -> Result<Vec<formula::ColRef>> {
        Ok(self
            .columns(sheet_id)?
            .into_iter()
            .map(|c| formula::ColRef { id: c.col_id, var: c.spec.var, display: c.spec.name })
            .collect())
    }

    /// Validate and normalise a column spec: display and variable names must be
    /// unique in the sheet; formulas are checked and stored with column IDs.
    fn check_spec(&self, sheet_id: &str, col_id: &str, spec: &ColumnSpec) -> Result<ColumnSpec> {
        let name = non_empty(&spec.name, "column name")?;
        if name.contains(['[', ']']) {
            return Err(Error::InvalidValue("display names cannot contain [ or ]".into()));
        }
        let others: Vec<ColumnInfo> = self.columns(sheet_id)?.into_iter().filter(|c| c.col_id != col_id).collect();
        if let Some(c) = others.iter().find(|c| c.spec.name.eq_ignore_ascii_case(&name)) {
            return Err(Error::InvalidValue(format!("column {} is already called {name}", c.col_id)));
        }
        let var = if spec.var.trim().is_empty() {
            let base = var_from_name(&name);
            let mut v = base.clone();
            let mut n = 2;
            while others.iter().any(|c| c.spec.var == v) {
                v = format!("{base}_{n}");
                n += 1;
            }
            v
        } else {
            let v = spec.var.trim().to_string();
            check_var(&v)?;
            if let Some(c) = others.iter().find(|c| c.spec.var == v) {
                return Err(Error::InvalidValue(format!("column {} already has the variable name {v}", c.col_id)));
            }
            v
        };
        let kind = match &spec.kind {
            ColumnKind::Calc { formula: src, currency } => {
                let expr = formula::parse(src).map_err(|e| Error::InvalidValue(format!("formula: {e}")))?;
                let mut cols = self.col_refs(sheet_id)?;
                cols.retain(|c| c.id != col_id);
                cols.push(formula::ColRef { id: col_id.to_string(), var: var.clone(), display: name.clone() });
                let resolved = formula::resolve(&expr, &cols).map_err(|e| Error::InvalidValue(format!("formula: {e}")))?;
                if formula::refs(&resolved).iter().any(|r| r == col_id) {
                    return Err(Error::InvalidValue(format!("the formula for {name} refers to itself")));
                }
                ColumnKind::Calc { formula: resolved.to_string(), currency: currency.clone() }
            }
            other => other.clone(),
        };
        Ok(ColumnSpec { name, var, kind, total: spec.total, style: spec.style.clone() })
    }

    fn write_spec(&self, sheet_id: &str, col_id: &str, spec: &ColumnSpec) -> Result<()> {
        self.run(
            "UPDATE columns SET name = ?, spec = ? WHERE sheet_id = ? AND col_id = ?",
            params![spec.name, serde_json::to_string(spec)?, sheet_id, col_id],
        )?;
        Ok(())
    }

    /// Convert a project from format 2 (columns had random IDs and formulas used
    /// display names) to format 3 (IDs c1, c2…, variable names, formulas stored by
    /// ID). The old log is kept in `ops-v2-backup/`.
    fn migrate_v2(dir: PathBuf, mut settings: ProjectSettings, machine: &str) -> Result<Self> {
        let probe = Self {
            dir: dir.clone(),
            machine: machine.to_string(),
            settings: settings.clone(),
            conn: Connection::open_in_memory()?,
            last_id: Ulid::nil(),
            entries_applied: 0,
            undo: vec![],
            redo: vec![],
            replay_warnings: vec![],
            functions: Default::default(),
            edited: HashMap::new(),
        };
        let (entries, _) = probe.read_log()?;
        drop(probe);

        let backup = dir.join("ops-v2-backup");
        if !backup.exists() {
            fs::rename(dir.join("ops"), &backup)?;
        } else {
            fs::remove_dir_all(dir.join("ops"))?;
        }
        fs::create_dir_all(dir.join("ops"))?;
        settings.format = FORMAT;
        fs::write(dir.join("project.json"), serde_json::to_string_pretty(&settings)?)?;
        let mut p = Self::open(&dir, machine)?;

        fn flatten(op: Op, out: &mut Vec<Op>) {
            match op {
                Op::Batch { ops } => ops.into_iter().for_each(|o| flatten(o, out)),
                o => out.push(o),
            }
        }
        let mut ops = Vec::new();
        for e in entries {
            flatten(e.op, &mut ops);
        }
        // old column ID -> new column ID, per sheet
        let mut ids: HashMap<(String, String), String> = HashMap::new();
        let mut warnings = Vec::new();
        for op in ops {
            let map = |ids: &HashMap<(String, String), String>, sheet: &str, col: &str| {
                ids.get(&(sheet.to_string(), col.to_string())).cloned().unwrap_or_else(|| col.to_string())
            };
            let legacy_spec = |p: &Project, sheet: &str, mut spec: ColumnSpec| -> Result<ColumnSpec> {
                if let ColumnKind::Calc { formula: src, currency } = &spec.kind {
                    let cols = p.col_refs(sheet)?;
                    let expr = formula::parse(src).map_err(Error::InvalidValue)?;
                    let resolved = formula::resolve_legacy(&expr, &cols).map_err(Error::InvalidValue)?;
                    spec.kind = ColumnKind::Calc { formula: resolved.to_string(), currency: currency.clone() };
                }
                Ok(spec)
            };
            let translated = match op {
                Op::AddColumn { sheet_id, col_id, position, spec } => {
                    let new_id = p.next_col_id(&sheet_id).unwrap_or_else(|_| col_id.clone());
                    ids.insert((sheet_id.clone(), col_id), new_id.clone());
                    legacy_spec(&p, &sheet_id, spec).map(|spec| Op::AddColumn { sheet_id, col_id: new_id, position, spec })
                }
                Op::UpdateColumn { sheet_id, col_id, spec } => {
                    let col_id = map(&ids, &sheet_id, &col_id);
                    legacy_spec(&p, &sheet_id, spec).map(|spec| Op::UpdateColumn { sheet_id, col_id, spec })
                }
                Op::MoveColumn { sheet_id, col_id, position } => {
                    Ok(Op::MoveColumn { col_id: map(&ids, &sheet_id, &col_id), sheet_id, position })
                }
                Op::SetColumnDeleted { sheet_id, col_id, deleted } => {
                    Ok(Op::SetColumnDeleted { col_id: map(&ids, &sheet_id, &col_id), sheet_id, deleted })
                }
                Op::SetCell { sheet_id, row_id, col_id, value } => {
                    Ok(Op::SetCell { col_id: map(&ids, &sheet_id, &col_id), sheet_id, row_id, value })
                }
                other => Ok(other),
            };
            match translated.and_then(|op| p.apply_logged(op)) {
                Ok(_) => {}
                Err(e) => warnings.push(format!("while converting to the new format: {e}")),
            }
        }
        p.undo.clear();
        p.replay_warnings.extend(warnings);
        Ok(p)
    }

    // ---------------------------------------------------------------- reading

    fn sheet_exists(&self, sheet_id: &str) -> Result<bool> {
        let n: i64 =
            self.qrow("SELECT count(*) FROM sheets WHERE sheet_id = ?", params![sheet_id], |r| r.get(0))?;
        Ok(n > 0)
    }

    /// (kind, deleted) of a row, if it exists.
    /// (row ID, value) of every stored cell in a column.
    fn column_cells(&self, sheet_id: &str, col_id: &str) -> Result<Vec<(String, String)>> {
        let mut st = self.conn.prepare_cached("SELECT row_id, value FROM cells WHERE sheet_id = ? AND col_id = ?")?;
        let rows = st.query_map(params![sheet_id, col_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    fn row_state(&self, sheet_id: &str, row_id: &str) -> Result<Option<(RowKind, bool)>> {
        Ok(self
            
            .qrow(
                "SELECT kind, deleted FROM sheet_rows WHERE sheet_id = ? AND row_id = ?",
                params![sheet_id, row_id],
                |r| Ok((RowKind::parse(&r.get::<_, String>(0)?), r.get::<_, bool>(1)?)),
            )
            .optional()?)
    }

    /// (spec json, deleted) of a column, deleted or not.
    fn column_row(&self, sheet_id: &str, col_id: &str) -> Result<Option<(String, bool)>> {
        Ok(self
            
            .qrow(
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
            
            .qrow(
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
        let mut stmt = self.conn.prepare_cached(
            "SELECT sheet_id, name, archived, rates, sources, fixed_rates, base FROM sheets ORDER BY position, sheet_id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, bool>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (sheet_id, name, archived, rates, sources, fixed, base) = row?;
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
                base: if base.is_empty() { None } else { Some(Currency::new(&base)?) },
            });
        }
        Ok(out)
    }

    /// Columns of a sheet in order, without deleted ones.
    pub fn columns(&self, sheet_id: &str) -> Result<Vec<ColumnInfo>> {
        let mut stmt = self.conn.prepare_cached(
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
        let mut stmt = self.conn.prepare_cached(
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
        let mut stmt = self.conn.prepare_cached("SELECT row_id, col_id, value FROM cells WHERE sheet_id = ?")?;
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
            self.qrow("SELECT max(position) FROM sheet_rows WHERE sheet_id = ?", params![sheet_id], |r| r.get(0))?;
        Ok(max.map_or(1.0, |m| m.floor() + 1.0))
    }

    /// A position after the last column of a sheet.
    pub fn next_column_position(&self, sheet_id: &str) -> Result<f64> {
        let max: Option<f64> =
            self.qrow("SELECT max(position) FROM columns WHERE sheet_id = ?", params![sheet_id], |r| r.get(0))?;
        Ok(max.map_or(1.0, |m| m.floor() + 1.0))
    }

    /// The operations that recreate a sheet as it is now, under `new_id`, in this or
    /// another project: its rate settings, columns (same IDs, names, types, formulas and
    /// order), rows (same order, sections included) and typed-in values. Deleted rows
    /// and columns are left behind. Apply them as one [`Op::Batch`] to get one undo step.
    pub fn copy_sheet_ops(&self, sheet_id: &str, new_id: &str) -> Result<Vec<Op>> {
        let info = self.sheet(sheet_id)?;
        let columns = self.columns(sheet_id)?;
        let rows = self.rows(sheet_id)?;
        let cells = self.cells(sheet_id)?;
        let s = || new_id.to_string();

        let mut ops = vec![Op::CreateSheet { sheet_id: s(), name: info.name.clone() }];
        if info.rates != RateMode::Live {
            ops.push(Op::SetSheetRates { sheet_id: s(), rates: info.rates.clone() });
        }
        if !info.sources.is_empty() {
            ops.push(Op::SetSheetSources { sheet_id: s(), sources: info.sources.clone() });
        }
        if info.base.is_some() {
            ops.push(Op::SetSheetBase { sheet_id: s(), base: info.base.clone() });
        }
        if !info.fixed_rates.is_empty() {
            ops.push(Op::SetSheetFixedRates { sheet_id: s(), rates: info.fixed_rates.clone() });
        }
        // Typed columns first, then calculated ones in an order where every column a
        // formula refers to already exists (a formula may refer to a column to its right).
        let mut added: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut pending: Vec<&ColumnInfo> = Vec::new();
        for c in &columns {
            if c.spec.kind.is_input() {
                added.insert(c.col_id.clone());
                ops.push(Op::AddColumn { sheet_id: s(), col_id: c.col_id.clone(), position: c.position, spec: c.spec.clone() });
            } else {
                pending.push(c);
            }
        }
        let col_refs = self.col_refs(sheet_id)?;
        let refs_of = |c: &ColumnInfo| -> Vec<String> {
            match &c.spec.kind {
                ColumnKind::Calc { formula: f, .. } => formula::parse(f)
                    .and_then(|e| formula::resolve(&e, &col_refs))
                    .map(|e| formula::refs(&e))
                    .unwrap_or_default(),
                _ => vec![],
            }
        };
        while !pending.is_empty() {
            let ready = pending.iter().position(|c| refs_of(c).iter().all(|r| added.contains(r) || !columns.iter().any(|x| &x.col_id == r)));
            // No column is ready only if formulas refer to each other in a loop; add the
            // next one anyway so the error names it.
            let c = pending.remove(ready.unwrap_or(0));
            added.insert(c.col_id.clone());
            ops.push(Op::AddColumn { sheet_id: s(), col_id: c.col_id.clone(), position: c.position, spec: c.spec.clone() });
        }
        // Calculated columns hold no values of their own.
        let typed: Vec<&str> = columns.iter().filter(|c| c.spec.kind.is_input()).map(|c| c.col_id.as_str()).collect();
        let new_rows: Vec<NewRow> = rows
            .iter()
            .map(|r| NewRow {
                row_id: r.row_id.clone(),
                position: r.position,
                cells: typed
                    .iter()
                    .filter_map(|c| cells.get(&(r.row_id.clone(), c.to_string())).map(|v| (c.to_string(), v.clone())))
                    .collect(),
            })
            .collect();
        if !new_rows.is_empty() {
            ops.push(Op::InsertRows { sheet_id: s(), rows: new_rows });
        }
        for r in rows.iter().filter(|r| r.kind != RowKind::Item) {
            ops.push(Op::SetRowKind { sheet_id: s(), row_id: r.row_id.clone(), kind: r.kind });
        }
        Ok(ops)
    }
}

/// A variable name from a display name: "Price (UAH)" -> price_uah.
pub fn var_from_name(name: &str) -> String {
    let mut out = String::new();
    for ch in name.trim().to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let out = out.trim_matches('_').to_string();
    let out = if out.is_empty() { "col".to_string() } else { out };
    let out = if out.starts_with(|c: char| c.is_ascii_digit()) || is_reserved_var(&out) { format!("col_{out}") } else { out };
    out
}

fn is_reserved_var(v: &str) -> bool {
    let id_like = |p: char| v.strip_prefix(p).is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
    id_like('c') || id_like('r') || formula::FUNCTIONS.contains(&v)
}

/// Variable names: lowercase letters, digits and `_`, starting with a letter; not
/// a column or row ID (`c4`, `r2`) or a function name.
pub fn check_var(v: &str) -> Result<()> {
    let ok = v.starts_with(|c: char| c.is_ascii_lowercase())
        && v.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !ok {
        return Err(Error::InvalidValue(format!(
            "variable name {v:?}: use lowercase letters, digits and _, starting with a letter"
        )));
    }
    if is_reserved_var(v) {
        return Err(Error::InvalidValue(format!("variable name {v:?} is reserved (column IDs, row IDs and functions)")));
    }
    Ok(())
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
        ColumnKind::Date => Ok(crate::time::store_date(&crate::time::parse_date(v)?)),
        ColumnKind::DateTime => Ok(crate::time::store_moment(&crate::time::parse_moment(v)?)),
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
        ColumnSpec { name: name.into(), var: String::new(), kind, total, style: Default::default() }
    }

    /// A project with one sheet "s" laid out like a spending list.
    pub fn spending_project(dir: &Path) -> Project {
        let mut p = Project::create(dir, ProjectSettings::new("Test", cur("GBP")), "mac").unwrap();
        let cols = [
            ("item", spec("Item", K::Text, false)),
            ("uah", spec("UAH", K::Money { currency: cur("UAH") }, false)),
            ("usd", spec("USD", K::Money { currency: cur("USD") }, false)),
            ("unit", spec("Unit USD", K::Calc { formula: "coalesce(to(uah, USD), usd)".into(), currency: None }, false)),
            ("qty", spec("Qty", K::Number, false)),
            ("total", spec("Total USD", K::Calc { formula: "[Unit USD] * qty".into(), currency: None }, true)),
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

        // Formulas are stored by ID, so renaming Unit USD changes nothing in Total USD.
        assert_eq!(p.column("s", "total").unwrap().spec.kind, K::Calc { formula: "unit * qty".into(), currency: None });
        p.apply(Op::UpdateColumn {
            sheet_id: "s".into(),
            col_id: "unit".into(),
            spec: ColumnSpec { name: "Each (USD)".into(), var: "each_usd".into(), kind: K::Calc { formula: "coalesce(to(uah, USD), usd)".into(), currency: None }, total: false , style: Default::default()},
        })
        .unwrap();
        assert_eq!(p.column("s", "total").unwrap().spec.kind, K::Calc { formula: "unit * qty".into(), currency: None });
        assert_eq!(p.column("s", "unit").unwrap().spec.var, "each_usd");

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
            spec: spec("X", K::Calc { formula: f.into(), currency: None }, false),
        };
        assert!(p.apply(add("qty *")).is_err());
        assert!(p.apply(add("x + 1")).is_err(), "refers to itself");
        assert!(p.apply(add("price * 2")).is_err(), "no such column");
        assert!(p.apply(add("= Qty * 2")).is_ok());
        assert_eq!(p.column("s", "x").unwrap().spec.kind, K::Calc { formula: "qty * 2".into(), currency: None });
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
    fn ids_and_variable_names() {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = spending_project(tmp.path());
        let add = |name: &str, var: &str| Op::AddColumn {
            sheet_id: "s".into(),
            col_id: String::new(),
            position: 50.0,
            spec: ColumnSpec { name: name.into(), var: var.into(), kind: K::Text, total: false , style: Default::default()},
        };
        p.apply(add("Price (EUR)", "")).unwrap();
        p.apply(add("Price (EUR) 2", "")).unwrap();
        let cols = p.columns("s").unwrap();
        let a = &cols[cols.len() - 2];
        let b = &cols[cols.len() - 1];
        assert_eq!((a.col_id.as_str(), a.spec.var.as_str()), ("c1", "price_eur"));
        assert_eq!((b.col_id.as_str(), b.spec.var.as_str()), ("c2", "price_eur_2"));
        // IDs are never reused, even after a delete.
        p.apply(Op::SetColumnDeleted { sheet_id: "s".into(), col_id: "c2".into(), deleted: true }).unwrap();
        p.apply(add("Other", "")).unwrap();
        assert!(p.columns("s").unwrap().iter().any(|c| c.col_id == "c3"));
        // Bad and reserved variable names.
        assert!(p.apply(add("Bad", "Bad Name")).is_err());
        assert!(p.apply(add("Bad", "c9")).is_err());
        assert!(p.apply(add("Bad", "round")).is_err());
        assert_eq!(var_from_name("Price (USD)"), "price_usd");
        assert_eq!(var_from_name("2024 total"), "col_2024_total");
        assert_eq!(var_from_name("C4"), "col_c4");
    }

    #[test]
    fn format_2_projects_are_converted() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("ops")).unwrap();
        fs::write(
            tmp.path().join("project.json"),
            r#"{"format":2,"name":"Old","reporting_currency":"GBP","rate_sources":["ecb","nbu"]}"#,
        )
        .unwrap();
        let money = |c: &str| format!(r#""type":"money","currency":"{c}""#);
        let lines = [
            r#"{"op":"batch","ops":[{"op":"create_sheet","sheet_id":"s","name":"Spending"},"#.to_string()
                + &format!(r#"{{"op":"add_column","sheet_id":"s","col_id":"01AAA","position":1.0,"spec":{{"name":"Item","type":"text","total":false}}}},"#)
                + &format!(r#"{{"op":"add_column","sheet_id":"s","col_id":"01BBB","position":2.0,"spec":{{"name":"UAH",{},"total":false}}}},"#, money("UAH"))
                + &format!(r#"{{"op":"add_column","sheet_id":"s","col_id":"01CCC","position":3.0,"spec":{{"name":"USD",{},"total":false}}}},"#, money("USD"))
                + r#"{"op":"add_column","sheet_id":"s","col_id":"01DDD","position":4.0,"spec":{"name":"Unit USD","type":"calc","formula":"coalesce(to(UAH, USD), USD)","total":false}},"#
                + r#"{"op":"add_column","sheet_id":"s","col_id":"01EEE","position":5.0,"spec":{"name":"Qty","type":"number","total":false}},"#
                + r#"{"op":"add_column","sheet_id":"s","col_id":"01FFF","position":6.0,"spec":{"name":"Total USD","type":"calc","formula":"[Unit USD] * Qty","total":true}}]}"#,
            r#"{"op":"insert_row","sheet_id":"s","row_id":"r1","position":1.0}"#.to_string(),
            r#"{"op":"set_cell","sheet_id":"s","row_id":"r1","col_id":"01BBB","value":"4400"}"#.to_string(),
            r#"{"op":"set_cell","sheet_id":"s","row_id":"r1","col_id":"01EEE","value":"2"}"#.to_string(),
        ];
        let log: String = lines
            .iter()
            .enumerate()
            .map(|(i, op)| format!(r#"{{"id":"01J00000000000000000000{i:03}","ts":"2026-10-22T10:00:00Z","machine":"mac","op":{op}}}"#) + "\n")
            .collect();
        fs::write(tmp.path().join("ops/mac.jsonl"), log).unwrap();

        let p = Project::open(tmp.path(), "mac").unwrap();
        assert!(p.replay_warnings().is_empty(), "{:?}", p.replay_warnings());
        let cols = p.columns("s").unwrap();
        let ids: Vec<_> = cols.iter().map(|c| (c.col_id.as_str(), c.spec.var.as_str())).collect();
        assert_eq!(ids, [("c1", "item"), ("c2", "uah"), ("c3", "usd"), ("c4", "unit_usd"), ("c5", "qty"), ("c6", "total_usd")]);
        assert_eq!(cols[3].spec.kind, K::Calc { formula: "coalesce(to(c2, USD), c3)".into(), currency: None });
        assert_eq!(cols[5].spec.kind, K::Calc { formula: "c4 * c5".into(), currency: None });
        assert_eq!(p.cells("s").unwrap()[&("r1".into(), "c2".into())], "4400");
        assert_eq!(read_settings(tmp.path()).unwrap().format, FORMAT);
        assert!(tmp.path().join("ops-v2-backup/mac.jsonl").exists());
        // Opens normally afterwards.
        drop(p);
        let again = Project::open(tmp.path(), "mac").unwrap();
        assert_eq!(again.columns("s").unwrap().len(), 6);
    }

    #[test]
    fn urls_get_a_scheme() {
        assert_eq!(normalize_url("rozetka.com.ua/item/1"), "https://rozetka.com.ua/item/1");
        assert_eq!(normalize_url("http://x.com"), "http://x.com");
        assert_eq!(normalize_url("ask Olena"), "ask Olena");
        let _ = dec!(1);
    }

    #[test]
    fn imported_rows_replay_and_undo() {
        let tmp = tempfile::tempdir().unwrap();
        {
            let mut p = Project::create(tmp.path(), ProjectSettings::new("t", Currency::new("GBP").unwrap()), "mac").unwrap();
            let t = crate::import::read_csv("name,price,qty\nA,₴1 200,2\nB,,3\n").unwrap();
            p.apply(Op::Batch { ops: crate::import::sheet_ops(&t, "s", "Imported") }).unwrap();
            assert_eq!(p.rows("s").unwrap().len(), 2);
            assert_eq!(p.cells("s").unwrap().len(), 5);
        }
        let mut p = Project::open(tmp.path(), "mac").unwrap();
        assert!(p.replay_warnings().is_empty(), "{:?}", p.replay_warnings());
        assert_eq!(p.rows("s").unwrap().len(), 2);
        let c = p.columns("s").unwrap();
        assert_eq!(c[1].spec.kind, ColumnKind::Money { currency: Currency::new("UAH").unwrap() });
        assert!(p.cells("s").unwrap().values().any(|v| v == "1200"));
        let t = crate::import::read_csv("x\n1\n2\n").unwrap();
        p.apply(Op::Batch { ops: crate::import::sheet_ops(&t, "s2", "Two") }).unwrap();
        assert_eq!(p.rows("s2").unwrap().len(), 2);
        assert!(p.undo().unwrap());
        assert!(p.sheet("s2").is_err() || p.rows("s2").unwrap().is_empty());
    }

    #[test]
    fn retyping_numbers_as_times_and_time_arithmetic() {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = Project::create(tmp.path(), ProjectSettings::new("t", Currency::new("GBP").unwrap()), "mac").unwrap();
        let t = crate::import::read_csv("start,end,day\n1789728376.5,1789728436,18.09.2026\n").unwrap();
        p.apply(Op::Batch { ops: crate::import::sheet_ops(&t, "s", "Flights") }).unwrap();
        let cols = p.columns("s").unwrap();
        assert_eq!(cols[0].spec.kind, ColumnKind::Number);
        assert_eq!(cols[2].spec.kind, ColumnKind::Date);
        for (id, name) in [("c1", "start"), ("c2", "end")] {
            p.apply(Op::UpdateColumn {
                sheet_id: "s".into(),
                col_id: id.into(),
                spec: ColumnSpec { name: name.into(), var: name.into(), kind: ColumnKind::DateTime, total: false, style: Default::default() },
            })
            .unwrap();
        }
        assert!(p.cells("s").unwrap().values().any(|v| v == "2026-09-18T10:46:16.500Z"), "{:?}", p.cells("s").unwrap());
        p.apply(Op::AddColumn {
            sheet_id: "s".into(),
            col_id: String::new(),
            position: 9.0,
            spec: ColumnSpec { name: "Secs".into(), var: String::new(), kind: ColumnKind::Calc { formula: "end - start".into(), currency: None }, total: false, style: Default::default() },
        })
        .unwrap();
        p.apply(Op::AddColumn {
            sheet_id: "s".into(),
            col_id: String::new(),
            position: 10.0,
            spec: ColumnSpec { name: "Next".into(), var: String::new(), kind: ColumnKind::Calc { formula: "day + 1".into(), currency: None }, total: false, style: Default::default() },
        })
        .unwrap();
        let v = p.sheet_view("s", &crate::fx::RateTable::default(), chrono::NaiveDate::from_ymd_opt(2026, 10, 1).unwrap()).unwrap();
        assert_eq!(v.rows[0].cells[3].value.as_deref(), Some("59.5"), "{:?}", v.warnings);
        assert_eq!(v.rows[0].cells[4].value.as_deref(), Some("2026-09-19"));
    }

    #[test]
    fn a_sheet_copies_into_another_project_as_it_is() {
        let tmp = tempfile::tempdir().unwrap();
        let mut src = spending_project(&tmp.path().join("a"));
        add_row(&mut src, "h", &[("item", "Frame")]);
        src.apply(Op::SetRowKind { sheet_id: "s".into(), row_id: "h".into(), kind: RowKind::Section }).unwrap();
        add_row(&mut src, "r1", &[("item", "Carbon plates"), ("uah", "3250"), ("qty", "2")]);
        add_row(&mut src, "r2", &[("item", "Gone"), ("usd", "5")]);
        src.apply(Op::SetRowDeleted { sheet_id: "s".into(), row_id: "r2".into(), deleted: true }).unwrap();
        src.apply(Op::SetSheetBase { sheet_id: "s".into(), base: Some(cur("USD")) }).unwrap();
        let fixed = vec![FixedRate { base: cur("USD"), quote: cur("UAH"), rate: "44.92".into() }];
        src.apply(Op::SetSheetFixedRates { sheet_id: "s".into(), rates: fixed.clone() }).unwrap();
        // A formula that refers to a column on its right.
        src.apply(Op::AddColumn {
            sheet_id: "s".into(),
            col_id: "double".into(),
            position: -1.0,
            spec: spec("Double", K::Calc { formula: "[Total USD] * 2".into(), currency: None }, false),
        })
        .unwrap();

        let mut dst = Project::create(tmp.path().join("b"), ProjectSettings::new("Other", cur("USD")), "mac").unwrap();
        let ops = src.copy_sheet_ops("s", "t").unwrap();
        dst.apply(Op::Batch { ops }).unwrap();

        let t = dst.sheet("t").unwrap();
        assert_eq!(t.name, "Drone");
        assert_eq!(t.base, Some(cur("USD")));
        assert_eq!(t.fixed_rates, fixed);
        assert_eq!(dst.columns("t").unwrap(), src.columns("s").unwrap());
        let rows: Vec<(String, RowKind)> = dst.rows("t").unwrap().into_iter().map(|r| (r.row_id, r.kind)).collect();
        assert_eq!(rows, vec![("h".to_string(), RowKind::Section), ("r1".to_string(), RowKind::Item)]);
        let cells = dst.cells("t").unwrap();
        assert_eq!(cells[&("r1".into(), "uah".into())], "3250");
        assert_eq!(cells[&("h".into(), "item".into())], "Frame");
        assert_eq!(cells.len(), 4, "{cells:?}");

        // One undo step removes the whole copy.
        assert!(dst.undo().unwrap());
        assert!(dst.sheets(false).unwrap().is_empty());
        dst.redo().unwrap();
        assert_eq!(dst.rows("t").unwrap().len(), 2);

        // The copy survives reopening.
        drop(dst);
        let dst = Project::open(tmp.path().join("b"), "mac").unwrap();
        assert!(dst.replay_warnings().is_empty(), "{:?}", dst.replay_warnings());
        assert_eq!(dst.rows("t").unwrap().len(), 2);
    }
}
