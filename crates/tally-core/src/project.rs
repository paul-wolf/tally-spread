//! A project: one area of work (e.g. "Office fit-out") with its own sheets and
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

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use chrono::Utc;
use duckdb::{params, Connection, OptionalExt};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::fx::RateSource;
use crate::money::{parse_amount, Currency, Rounding, RoundingScope};
use crate::ops::{Column, LogEntry, Op, RateMode};
use crate::{Error, Result};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ProjectSettings {
    pub name: String,
    /// The currency the records are kept in (e.g. GBP for UK tax).
    pub reporting_currency: Currency,
    /// Default display currencies for new sheets.
    pub display_currencies: Vec<Currency>,
    /// Rate sources in order of preference.
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
            name: name.to_string(),
            display_currencies: vec![reporting.clone()],
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
    /// Display currencies set on the sheet (empty = project default).
    pub currencies: Vec<Currency>,
    pub rates: RateMode,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct CostRow {
    pub row_id: String,
    pub position: f64,
    pub item: Option<String>,
    pub url: Option<String>,
    pub qty: Option<Decimal>,
    pub unit_price: Option<Decimal>,
    pub currency: Option<Currency>,
    pub note: Option<String>,
}

const SCHEMA: &str = "
CREATE TABLE sheets (
    sheet_id VARCHAR NOT NULL,
    name VARCHAR NOT NULL,
    position INTEGER NOT NULL,
    archived BOOLEAN NOT NULL DEFAULT false,
    currencies VARCHAR NOT NULL DEFAULT '',
    rates VARCHAR NOT NULL DEFAULT 'live'
);
CREATE TABLE cost_items (
    sheet_id VARCHAR NOT NULL,
    row_id VARCHAR NOT NULL,
    position DOUBLE NOT NULL,
    item VARCHAR,
    url VARCHAR,
    qty DECIMAL(28, 8),
    unit_price DECIMAL(28, 8),
    currency VARCHAR,
    note VARCHAR,
    deleted BOOLEAN NOT NULL DEFAULT false
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

impl Project {
    /// Create a new project folder.
    pub fn create(dir: impl Into<PathBuf>, settings: ProjectSettings, machine: &str) -> Result<Self> {
        let dir = dir.into();
        if dir.join("project.json").exists() {
            return Err(Error::InvalidValue(format!("a project already exists in {}", dir.display())));
        }
        fs::create_dir_all(dir.join("ops"))?;
        fs::write(dir.join("project.json"), serde_json::to_string_pretty(&settings)?)?;
        Self::open(dir, machine)
    }

    /// Open a project folder and replay its logs.
    pub fn open(dir: impl Into<PathBuf>, machine: &str) -> Result<Self> {
        let dir = dir.into();
        let settings: ProjectSettings = serde_json::from_str(&fs::read_to_string(dir.join("project.json"))?)?;
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

    pub fn save_settings(&mut self, settings: ProjectSettings) -> Result<()> {
        fs::write(self.dir.join("project.json"), serde_json::to_string_pretty(&settings)?)?;
        self.settings = settings;
        Ok(())
    }

    /// The project's DuckDB connection, for SQL queries over `sheets` and `cost_items`.
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
        self.conn.execute_batch("DROP TABLE IF EXISTS sheets; DROP TABLE IF EXISTS cost_items;")?;
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
                let position: i64 =
                    self.conn.query_row("SELECT count(*) FROM sheets", [], |r| r.get(0))?;
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
                self.conn.execute(
                    "UPDATE sheets SET archived = ? WHERE sheet_id = ?",
                    params![archived, sheet_id],
                )?;
                Ok((op.clone(), Op::SetSheetArchived { sheet_id: sheet_id.clone(), archived: old }))
            }
            Op::SetSheetCurrencies { sheet_id, currencies } => {
                let old = self.sheet(sheet_id)?.currencies;
                let mut unique: Vec<Currency> = Vec::new();
                for c in currencies {
                    if !unique.contains(c) {
                        unique.push(c.clone());
                    }
                }
                let joined = unique.iter().map(|c| c.code()).collect::<Vec<_>>().join(",");
                self.conn.execute(
                    "UPDATE sheets SET currencies = ? WHERE sheet_id = ?",
                    params![joined, sheet_id],
                )?;
                Ok((
                    Op::SetSheetCurrencies { sheet_id: sheet_id.clone(), currencies: unique },
                    Op::SetSheetCurrencies { sheet_id: sheet_id.clone(), currencies: old },
                ))
            }
            Op::SetSheetRates { sheet_id, rates } => {
                let old = self.sheet(sheet_id)?.rates;
                self.conn.execute(
                    "UPDATE sheets SET rates = ? WHERE sheet_id = ?",
                    params![rates.to_db(), sheet_id],
                )?;
                Ok((op.clone(), Op::SetSheetRates { sheet_id: sheet_id.clone(), rates: old }))
            }
            Op::InsertRow { sheet_id, row_id, position } => {
                self.sheet(sheet_id)?;
                if self.row_exists(sheet_id, row_id)? {
                    return Err(Error::InvalidValue(format!("row {row_id} already exists")));
                }
                if !position.is_finite() {
                    return Err(Error::InvalidValue("row position must be a number".into()));
                }
                self.conn.execute(
                    "INSERT INTO cost_items (sheet_id, row_id, position) VALUES (?, ?, ?)",
                    params![sheet_id, row_id, position],
                )?;
                Ok((
                    op.clone(),
                    Op::SetRowDeleted { sheet_id: sheet_id.clone(), row_id: row_id.clone(), deleted: true },
                ))
            }
            Op::SetRowDeleted { sheet_id, row_id, deleted } => {
                let old: bool = self
                    .conn
                    .query_row(
                        "SELECT deleted FROM cost_items WHERE sheet_id = ? AND row_id = ?",
                        params![sheet_id, row_id],
                        |r| r.get(0),
                    )
                    .optional()?
                    .ok_or_else(|| Error::NotFound(format!("row {row_id}")))?;
                self.conn.execute(
                    "UPDATE cost_items SET deleted = ? WHERE sheet_id = ? AND row_id = ?",
                    params![deleted, sheet_id, row_id],
                )?;
                Ok((
                    op.clone(),
                    Op::SetRowDeleted { sheet_id: sheet_id.clone(), row_id: row_id.clone(), deleted: old },
                ))
            }
            Op::SetCell { sheet_id, row_id, column, value } => {
                self.exec_set_cell(sheet_id, row_id, *column, value.as_deref())
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

    fn exec_set_cell(&self, sheet_id: &str, row_id: &str, column: Column, value: Option<&str>) -> Result<(Op, Op)> {
        if !self.row_exists(sheet_id, row_id)? {
            return Err(Error::NotFound(format!("row {row_id}")));
        }
        let old = self.cell(sheet_id, row_id, column)?;
        let value = value.map(str::trim).filter(|v| !v.is_empty());

        // An amount typed with a currency ("₴1 200", "£12.50") also sets the row's
        // currency when it differs.
        let mut also_currency: Option<Currency> = None;
        let normalized: Option<String> = match (column, value) {
            (_, None) => None,
            (Column::Qty, Some(v)) => Some(parse_amount(v)?.0.to_string()),
            (Column::UnitPrice, Some(v)) => {
                let (amount, cur) = parse_amount(v)?;
                if let Some(cur) = cur {
                    let row_cur = self.cell(sheet_id, row_id, Column::Currency)?;
                    if row_cur.as_deref() != Some(cur.code()) {
                        also_currency = Some(cur);
                    }
                }
                Some(amount.to_string())
            }
            (Column::Currency, Some(v)) => Some(Currency::new(v)?.code().to_string()),
            (Column::Url, Some(v)) => Some(normalize_url(v)),
            (_, Some(v)) => Some(v.to_string()),
        };

        let sql = if column.is_decimal() {
            format!(
                "UPDATE cost_items SET {} = CAST(? AS DECIMAL(28, 8)) WHERE sheet_id = ? AND row_id = ?",
                column.sql_name()
            )
        } else {
            format!("UPDATE cost_items SET {} = ? WHERE sheet_id = ? AND row_id = ?", column.sql_name())
        };
        self.conn.execute(&sql, params![normalized, sheet_id, row_id])?;

        let set = Op::SetCell { sheet_id: sheet_id.into(), row_id: row_id.into(), column, value: normalized };
        let unset = Op::SetCell { sheet_id: sheet_id.into(), row_id: row_id.into(), column, value: old };
        match also_currency {
            None => Ok((set, unset)),
            Some(cur) => {
                let (cur_set, cur_unset) =
                    self.exec_set_cell(sheet_id, row_id, Column::Currency, Some(cur.code()))?;
                Ok((Op::Batch { ops: vec![set, cur_set] }, Op::Batch { ops: vec![cur_unset, unset] }))
            }
        }
    }

    // ---------------------------------------------------------------- reading

    fn sheet_exists(&self, sheet_id: &str) -> Result<bool> {
        let n: i64 =
            self.conn.query_row("SELECT count(*) FROM sheets WHERE sheet_id = ?", params![sheet_id], |r| r.get(0))?;
        Ok(n > 0)
    }

    fn row_exists(&self, sheet_id: &str, row_id: &str) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT count(*) FROM cost_items WHERE sheet_id = ? AND row_id = ?",
            params![sheet_id, row_id],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    fn cell(&self, sheet_id: &str, row_id: &str, column: Column) -> Result<Option<String>> {
        let sql = format!(
            "SELECT CAST({} AS VARCHAR) FROM cost_items WHERE sheet_id = ? AND row_id = ?",
            column.sql_name()
        );
        let v: Option<String> = self.conn.query_row(&sql, params![sheet_id, row_id], |r| r.get(0))?;
        Ok(match v {
            Some(s) if column.is_decimal() => Some(normalize_decimal(&s)?),
            other => other,
        })
    }

    pub fn sheet(&self, sheet_id: &str) -> Result<SheetInfo> {
        self.sheets(true)?
            .into_iter()
            .find(|s| s.sheet_id == sheet_id)
            .ok_or_else(|| Error::NotFound(format!("sheet {sheet_id}")))
    }

    pub fn sheets(&self, include_archived: bool) -> Result<Vec<SheetInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT sheet_id, name, archived, currencies, rates FROM sheets ORDER BY position, sheet_id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, bool>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (sheet_id, name, archived, currencies, rates) = row?;
            if archived && !include_archived {
                continue;
            }
            let currencies = currencies
                .split(',')
                .filter(|c| !c.is_empty())
                .map(Currency::new)
                .collect::<Result<Vec<_>>>()?;
            out.push(SheetInfo { sheet_id, name, archived, currencies, rates: RateMode::from_db(&rates)? });
        }
        Ok(out)
    }

    /// Display currencies of a sheet, falling back to the project's.
    pub fn display_currencies(&self, sheet: &SheetInfo) -> Vec<Currency> {
        if sheet.currencies.is_empty() {
            self.settings.display_currencies.clone()
        } else {
            sheet.currencies.clone()
        }
    }

    /// Rows of a sheet in order, without deleted rows.
    pub fn rows(&self, sheet_id: &str) -> Result<Vec<CostRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT row_id, position, item, url, CAST(qty AS VARCHAR), CAST(unit_price AS VARCHAR), currency, note
             FROM cost_items WHERE sheet_id = ? AND NOT deleted ORDER BY position, row_id",
        )?;
        let rows = stmt.query_map(params![sheet_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, f64>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<String>>(7)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (row_id, position, item, url, qty, unit_price, currency, note) = row?;
            out.push(CostRow {
                row_id,
                position,
                item,
                url,
                qty: qty.map(|s| parse_decimal(&s)).transpose()?,
                unit_price: unit_price.map(|s| parse_decimal(&s)).transpose()?,
                currency: currency.map(|c| Currency::new(&c)).transpose()?,
                note,
            });
        }
        Ok(out)
    }

    /// A position after the last row of a sheet.
    pub fn next_position(&self, sheet_id: &str) -> Result<f64> {
        let max: Option<f64> = self.conn.query_row(
            "SELECT max(position) FROM cost_items WHERE sheet_id = ?",
            params![sheet_id],
            |r| r.get(0),
        )?;
        Ok(max.map_or(1.0, |m| m.floor() + 1.0))
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

/// `rozetka.com.ua/x` becomes `https://rozetka.com.ua/x`; anything else is kept as typed.
fn normalize_url(v: &str) -> String {
    let looks_like_host = v.split('/').next().is_some_and(|h| h.contains('.') && !h.contains(' '));
    if !v.contains("://") && looks_like_host && !v.contains(' ') {
        format!("https://{v}")
    } else {
        v.to_string()
    }
}

fn parse_decimal(s: &str) -> Result<Decimal> {
    Decimal::from_str(s.trim()).map(|d| d.normalize()).map_err(|_| Error::InvalidAmount(s.to_string()))
}

fn normalize_decimal(s: &str) -> Result<String> {
    Ok(parse_decimal(s)?.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn new_project(dir: &Path) -> Project {
        let s = ProjectSettings::new("Test", Currency::new("GBP").unwrap());
        Project::create(dir, s, "mac").unwrap()
    }

    fn add_row(p: &mut Project, sheet: &str, row: &str) {
        let pos = p.next_position(sheet).unwrap();
        p.apply(Op::InsertRow { sheet_id: sheet.into(), row_id: row.into(), position: pos }).unwrap();
    }

    fn set(p: &mut Project, row: &str, column: Column, v: &str) {
        p.apply(Op::SetCell { sheet_id: "s1".into(), row_id: row.into(), column, value: Some(v.into()) })
            .unwrap();
    }

    #[test]
    fn edits_normalise_and_persist_across_reopen() {
        let tmp = tempfile::tempdir().unwrap();
        {
            let mut p = new_project(tmp.path());
            p.apply(Op::CreateSheet { sheet_id: "s1".into(), name: "Desk setup".into() }).unwrap();
            add_row(&mut p, "s1", "r1");
            set(&mut p, "r1", Column::Item, "Monitor");
            set(&mut p, "r1", Column::UnitPrice, "₴12 499,50");
            set(&mut p, "r1", Column::Qty, "2");
        }
        let p = Project::open(tmp.path(), "mac").unwrap();
        assert!(p.replay_warnings().is_empty(), "{:?}", p.replay_warnings());
        let rows = p.rows("s1").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].item.as_deref(), Some("Monitor"));
        assert_eq!(rows[0].unit_price, Some(dec!(12499.5)));
        assert_eq!(rows[0].currency.as_ref().map(|c| c.code()), Some("UAH"), "currency taken from ₴");
        assert_eq!(rows[0].qty, Some(dec!(2)));
    }

    #[test]
    fn urls_get_a_scheme() {
        assert_eq!(normalize_url("rozetka.com.ua/item/1"), "https://rozetka.com.ua/item/1");
        assert_eq!(normalize_url("http://x.com"), "http://x.com");
        assert_eq!(normalize_url("ask Olena"), "ask Olena");
    }

    #[test]
    fn undo_and_redo() {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = new_project(tmp.path());
        p.apply(Op::CreateSheet { sheet_id: "s1".into(), name: "List".into() }).unwrap();
        add_row(&mut p, "s1", "r1");
        set(&mut p, "r1", Column::UnitPrice, "£10");
        set(&mut p, "r1", Column::UnitPrice, "12");

        assert!(p.undo().unwrap());
        assert_eq!(p.rows("s1").unwrap()[0].unit_price, Some(dec!(10)));
        assert!(p.undo().unwrap()); // price and currency together
        let r = &p.rows("s1").unwrap()[0];
        assert_eq!((r.unit_price, r.currency.clone()), (None, None));
        assert!(p.redo().unwrap());
        assert_eq!(p.rows("s1").unwrap()[0].currency.as_ref().unwrap().code(), "GBP");

        assert!(p.undo().unwrap());
        assert!(p.undo().unwrap()); // row insert
        assert!(p.rows("s1").unwrap().is_empty());
    }

    #[test]
    fn failed_ops_change_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = new_project(tmp.path());
        p.apply(Op::CreateSheet { sheet_id: "s1".into(), name: "List".into() }).unwrap();
        add_row(&mut p, "s1", "r1");
        let bad = Op::Batch {
            ops: vec![
                Op::SetCell { sheet_id: "s1".into(), row_id: "r1".into(), column: Column::Item, value: Some("Chair".into()) },
                Op::SetCell { sheet_id: "s1".into(), row_id: "r1".into(), column: Column::Qty, value: Some("lots".into()) },
            ],
        };
        assert!(p.apply(bad).is_err());
        assert_eq!(p.rows("s1").unwrap()[0].item, None);
        let reopened = Project::open(tmp.path(), "mac").unwrap();
        assert_eq!(reopened.rows("s1").unwrap()[0].item, None);
    }

    #[test]
    fn merges_logs_from_two_machines() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mac = new_project(tmp.path());
        mac.apply(Op::CreateSheet { sheet_id: "s1".into(), name: "List".into() }).unwrap();
        add_row(&mut mac, "s1", "r1");

        let mut hub = Project::open(tmp.path(), "hub").unwrap();
        hub.apply(Op::InsertRow { sheet_id: "s1".into(), row_id: "r2".into(), position: 2.0 }).unwrap();

        assert!(mac.refresh().unwrap());
        assert_eq!(mac.rows("s1").unwrap().len(), 2);
        assert!(!mac.refresh().unwrap());
        assert!(mac.replay_warnings().is_empty());
    }

    #[test]
    fn sql_access() {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = new_project(tmp.path());
        p.apply(Op::CreateSheet { sheet_id: "s1".into(), name: "List".into() }).unwrap();
        add_row(&mut p, "s1", "r1");
        set(&mut p, "r1", Column::UnitPrice, "19.99 EUR");
        let total: String = p
            .connection()
            .query_row("SELECT CAST(sum(unit_price) AS VARCHAR) FROM cost_items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(normalize_decimal(&total).unwrap(), "19.99");
    }
}
