//! Importing a file as new sheets: CSV (any delimiter), JSON, or an Excel
//! workbook (XLSX, XLSM, XLSB, older XLS, and ODS), one sheet per worksheet. The
//! file's headings become the columns, and each column's type is worked out from
//! its values, so a file with its own layout comes in typed and ready to use.
//! From Excel only values come in: a formula brings its last calculated result.

use std::str::FromStr;

use rust_decimal::Decimal;
use serde_json::Value;

use crate::money::{parse_amount, Currency};
use crate::ops::{ColumnKind, ColumnSpec, NewRow, Op};
use crate::{Error, Result};

/// A table read from a file: headings and rows of text (`None` = empty).
#[derive(Debug, Default)]
pub struct Table {
    pub headings: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
}

fn cell(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// Read CSV, guessing the delimiter (`,` `;` or tab) from the first line.
pub fn read_csv(text: &str) -> Result<Table> {
    let text = text.trim_start_matches('\u{feff}');
    let first = text.lines().next().unwrap_or("");
    let delim = [b',', b';', b'\t']
        .into_iter()
        .max_by_key(|d| first.matches(*d as char).count())
        .unwrap_or(b',');
    let mut r = csv::ReaderBuilder::new().delimiter(delim).has_headers(false).flexible(true).from_reader(text.as_bytes());
    let mut records: Vec<Vec<Option<String>>> = Vec::new();
    for rec in r.records() {
        let rec = rec.map_err(|e| Error::parse("CSV", e))?;
        records.push(rec.iter().map(cell).collect());
    }
    if records.is_empty() {
        return Err(Error::InvalidValue("the file is empty".into()));
    }
    Ok(from_records(records))
}

/// Headings and rows from records: the first record is headings unless it looks
/// like data (all numbers). Rows are padded to one width; empty rows are dropped.
fn from_records(mut records: Vec<Vec<Option<String>>>) -> Table {
    if records.is_empty() {
        return Table::default();
    }
    let has_headings = records[0].iter().flatten().any(|v| number(v).is_none());
    let width = records.iter().map(|r| r.len()).max().unwrap_or(0);
    let headings = if has_headings {
        let h = records.remove(0);
        (0..width).map(|i| h.get(i).cloned().flatten().unwrap_or_else(|| format!("Column {}", i + 1))).collect()
    } else {
        (0..width).map(|i| format!("Column {}", i + 1)).collect()
    };
    let rows = records.into_iter().map(|mut r| {
        r.resize(width, None);
        r
    });
    Table { headings, rows: rows.filter(|r| r.iter().any(|c| c.is_some())).collect() }
}

/// Read JSON: an array of objects (keys become columns), or a Tally JSON export.
pub fn read_json(text: &str) -> Result<Table> {
    let v: Value = serde_json::from_str(text)?;
    let items: Vec<&Value> = match &v {
        Value::Array(a) => a.iter().collect(),
        Value::Object(o) => match o.get("rows") {
            // Tally's own export: rows with values by variable name.
            Some(Value::Array(rows)) => rows.iter().map(|r| r.get("values").unwrap_or(r)).collect(),
            _ => o.values().find_map(|x| x.as_array()).map(|a| a.iter().collect()).unwrap_or_default(),
        },
        _ => vec![],
    };
    let mut headings: Vec<String> = Vec::new();
    for it in &items {
        if let Value::Object(o) = it {
            for k in o.keys() {
                if !headings.contains(k) {
                    headings.push(k.clone());
                }
            }
        }
    }
    if headings.is_empty() {
        return Err(Error::InvalidValue("no records found: expected a list of objects".into()));
    }
    let text_of = |v: &Value| -> Option<String> {
        match v {
            Value::Null => None,
            Value::String(s) => cell(s),
            Value::Number(n) => Some(n.to_string()),
            Value::Bool(b) => Some(b.to_string()),
            // {"amount": 77.85, "currency": "USD"} from a Tally export.
            Value::Object(o) if o.contains_key("amount") => {
                let a = o.get("amount").map(|a| a.to_string().trim_matches('"').to_string())?;
                Some(match o.get("currency").and_then(|c| c.as_str()) {
                    Some(c) => format!("{a} {c}"),
                    None => a,
                })
            }
            other => Some(other.to_string()),
        }
    };
    let rows = items
        .iter()
        .map(|it| headings.iter().map(|h| it.get(h).and_then(text_of)).collect())
        .collect();
    Ok(Table { headings, rows })
}

/// A number from Excel as text. Excel shows 15 significant digits and binary
/// floats carry noise beyond them (0.1 + 0.2 is stored as 0.30000000000000004),
/// so the value is rounded to 15 digits before it becomes an exact decimal.
fn excel_number(f: f64) -> String {
    if !f.is_finite() {
        return f.to_string();
    }
    if f.fract() == 0.0 && f.abs() < 1e15 {
        return format!("{}", f as i64);
    }
    Decimal::from_scientific(&format!("{f:.14e}")).map(|d| d.normalize().to_string()).unwrap_or_else(|_| f.to_string())
}

/// A cell's value as text, or `None` when it is empty or an error (`#N/A`, …).
fn excel_cell(d: &calamine::Data) -> Option<String> {
    use calamine::Data;
    match d {
        Data::Empty | Data::Error(_) => None,
        Data::String(s) => cell(s),
        Data::Int(i) => Some(i.to_string()),
        Data::Float(f) => Some(excel_number(*f)),
        Data::Bool(b) => Some(b.to_string()),
        Data::DateTimeIso(s) | Data::DurationIso(s) => cell(s),
        Data::DateTime(dt) if dt.is_duration() => Some(excel_number(dt.as_f64())),
        Data::DateTime(dt) => {
            let t = dt.as_datetime()?;
            // A whole day is a date; anything else a local date and time.
            Some(if t.time() == chrono::NaiveTime::MIN {
                t.date().format("%Y-%m-%d").to_string()
            } else if t.format("%S").to_string() == "00" {
                t.format("%Y-%m-%d %H:%M").to_string()
            } else {
                t.format("%Y-%m-%d %H:%M:%S").to_string()
            })
        }
    }
}

/// A row that sums the rows above it (`=SUM(B2:B9)`, `=SUBTOTAL(9,C2:C20)`):
/// Tally totals columns itself, so a hand-built totals row is left out rather
/// than imported as data. A sum along its own row (`=SUM(B2:D2)`) is ordinary.
fn is_totals_formula(f: &str) -> bool {
    let f = f.trim_start_matches('=').trim().to_ascii_uppercase();
    if !["SUM(", "SUBTOTAL(", "AGGREGATE("].iter().any(|p| f.starts_with(p)) {
        return false;
    }
    // Any range whose two ends are on different rows.
    let row_of = |r: &str| -> Option<u32> {
        let r = r.rsplit('!').next()?.replace('$', "");
        let digits: String = r.chars().skip_while(|c| c.is_ascii_alphabetic()).collect();
        digits.parse().ok()
    };
    f.split(|c: char| matches!(c, '(' | ')' | ',' | ';' | ' ')).any(|part| match part.split_once(':') {
        Some((a, b)) => matches!((row_of(a), row_of(b)), (Some(x), Some(y)) if x != y),
        None => false,
    })
}

/// One worksheet as a table. Title lines above the headings (a heading row is
/// the first that fills at least half the columns) are skipped, so are totals
/// rows and columns with neither a heading nor any values.
fn worksheet_table(cells: &calamine::Range<calamine::Data>, formulas: Option<&calamine::Range<String>>) -> Table {
    let Some((r0, c0)) = cells.start() else { return Table::default() };
    let mut records: Vec<Vec<Option<String>>> = Vec::new();
    for (i, row) in cells.rows().enumerate() {
        let totals = formulas.is_some_and(|fr| {
            (0..row.len()).any(|j| fr.get_value((r0 + i as u32, c0 + j as u32)).is_some_and(|f| is_totals_formula(f)))
        });
        if totals {
            continue;
        }
        records.push(row.iter().map(excel_cell).collect());
    }
    while records.last().is_some_and(|r| r.iter().all(|c| c.is_none())) {
        records.pop();
    }
    let width = records.iter().map(|r| r.len()).max().unwrap_or(0);
    let filled = |r: &Vec<Option<String>>| r.iter().filter(|c| c.is_some()).count();
    let need = if width <= 1 { 1 } else { width.div_ceil(2).max(2) };
    if let Some(first) = records.iter().position(|r| filled(r) >= need) {
        records.drain(..first);
    }
    let mut t = from_records(records);
    // Drop columns that are entirely empty and have no heading of their own.
    let keep: Vec<usize> = (0..t.headings.len())
        .filter(|&j| !t.headings[j].starts_with("Column ") || t.rows.iter().any(|r| r[j].is_some()))
        .collect();
    if keep.len() < t.headings.len() {
        t.headings = keep.iter().map(|&j| t.headings[j].clone()).collect();
        t.rows = t.rows.into_iter().map(|r| keep.iter().map(|&j| r[j].clone()).collect()).collect();
    }
    t
}

/// Read every worksheet of a workbook that holds data, by worksheet name.
pub fn read_workbook(path: &std::path::Path) -> Result<Vec<(String, Table)>> {
    use calamine::Reader;
    let bad = |e: calamine::Error| Error::parse("Excel", e);
    let mut wb = calamine::open_workbook_auto(path).map_err(bad)?;
    let mut out = Vec::new();
    for name in wb.sheet_names() {
        let cells = wb.worksheet_range(&name).map_err(bad)?;
        let formulas = wb.worksheet_formula(&name).ok();
        let t = worksheet_table(&cells, formulas.as_ref());
        if !t.headings.is_empty() && !t.rows.is_empty() {
            out.push((name, t));
        }
    }
    if out.is_empty() {
        return Err(Error::InvalidValue("the workbook has no worksheets with data".into()));
    }
    Ok(out)
}

fn number(s: &str) -> Option<Decimal> {
    Decimal::from_str(s).ok().or_else(|| Decimal::from_scientific(s).ok())
}

fn is_link(s: &str) -> bool {
    let l = s.to_ascii_lowercase();
    (l.starts_with("http://") || l.starts_with("https://") || l.starts_with("www.")) && !l.contains(' ')
}

/// Headings that say a column of numbers holds times: `ts`, `time`, `timestamp`,
/// `created_at`, `epoch`, `start_ts`, ….
fn sounds_like_time(heading: &str) -> bool {
    let h = heading.trim().to_ascii_lowercase().replace([' ', '-'], "_");
    let words: Vec<&str> = h.split('_').filter(|w| !w.is_empty()).collect();
    words.iter().any(|w| matches!(*w, "ts" | "time" | "timestamp" | "epoch" | "datetime" | "date"))
        || h.ends_with("_at")
        || h.ends_with("time")
}

/// The type a column's values suggest: numbers (or Unix times, when the heading
/// says so), dates, dates and times, amounts in one currency, links or text.
pub fn infer_kind(heading: &str, values: &[&str]) -> ColumnKind {
    use crate::time::{from_epoch, parse_date, parse_moment};
    if values.is_empty() {
        return ColumnKind::Text;
    }
    if values.iter().all(|v| number(v).is_some()) {
        let epoch = |v: &&str| v.parse::<f64>().ok().and_then(from_epoch).is_some();
        if sounds_like_time(heading) && values.iter().all(epoch) {
            return ColumnKind::DateTime;
        }
        return ColumnKind::Number;
    }
    if values.iter().all(|v| !v.contains(':') && parse_date(v).is_ok()) {
        return ColumnKind::Date;
    }
    if values.iter().all(|v| v.contains(':') && parse_moment(v).is_ok()) {
        return ColumnKind::DateTime;
    }
    if values.iter().all(|v| is_link(v)) {
        return ColumnKind::Link;
    }
    // Amounts like "₴1 200" or "12.50 EUR": money, when they all name the same currency.
    let mut currency: Option<Currency> = None;
    let all_amounts = values.iter().all(|v| match parse_amount(v) {
        Ok((_, Some(c))) => match &currency {
            None => {
                currency = Some(c);
                true
            }
            Some(have) => *have == c,
        },
        Ok((_, None)) => true,
        Err(_) => false,
    });
    match currency {
        Some(currency) if all_amounts => ColumnKind::Money { currency },
        _ => ColumnKind::Text,
    }
}

/// The operations that create a sheet holding the table, as one undoable step.
pub fn sheet_ops(table: &Table, sheet_id: &str, name: &str) -> Vec<Op> {
    let mut ops = vec![Op::CreateSheet { sheet_id: sheet_id.into(), name: name.into() }];
    // Headings must be unique display names.
    let mut seen: Vec<String> = Vec::new();
    let names: Vec<String> = table
        .headings
        .iter()
        .map(|h| {
            let mut n = h.clone();
            let mut k = 2;
            while seen.iter().any(|s| s.eq_ignore_ascii_case(&n)) {
                n = format!("{h} {k}");
                k += 1;
            }
            seen.push(n.clone());
            n
        })
        .collect();
    for (j, name) in names.iter().enumerate() {
        let values: Vec<&str> = table.rows.iter().filter_map(|r| r.get(j).and_then(|c| c.as_deref())).collect();
        let kind = infer_kind(&table.headings[j], &values);
        let total = matches!(kind, ColumnKind::Money { .. });
        ops.push(Op::AddColumn {
            sheet_id: sheet_id.into(),
            col_id: format!("c{}", j + 1),
            position: (j + 1) as f64,
            spec: ColumnSpec { name: name.clone(), var: String::new(), kind, total, style: Default::default() },
        });
    }
    let rows = table
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| NewRow {
            row_id: Op::new_id(),
            position: (i + 1) as f64,
            cells: row.iter().enumerate().filter_map(|(j, v)| v.clone().map(|v| (format!("c{}", j + 1), v))).collect(),
        })
        .collect();
    ops.push(Op::InsertRows { sheet_id: sheet_id.into(), rows });
    ops
}

const EXCEL: [&str; 5] = ["xlsx", "xlsm", "xlsb", "xls", "ods"];

/// Read a file by its extension: one table, or one per worksheet of a workbook,
/// each with the name its sheet should get.
pub fn read_tables(path: &std::path::Path) -> Result<Vec<(String, Table)>> {
    let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase());
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Imported").to_string();
    if ext.as_deref().is_some_and(|e| EXCEL.contains(&e)) {
        let sheets = read_workbook(path)?;
        // A single worksheet takes the file's name; several keep their own.
        return Ok(if sheets.len() == 1 { sheets.into_iter().map(|(_, t)| (stem.clone(), t)).collect() } else { sheets });
    }
    Ok(vec![(stem, read_file(path)?)])
}

/// Read a CSV or JSON file by its extension.
pub fn read_file(path: &std::path::Path) -> Result<Table> {
    match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some(e) if EXCEL.contains(&e) => Ok(read_workbook(path)?.into_iter().next().map(|(_, t)| t).unwrap_or_default()),
        Some("json") => read_json(&std::fs::read_to_string(path)?),
        Some("csv" | "tsv" | "txt") | None => read_csv(&std::fs::read_to_string(path)?),
        Some(other) => Err(Error::InvalidValue(format!("can't import .{other} files: use Excel, CSV or JSON"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_types() {
        let t = read_csv("name;price;link;n;flag\nA;₴1 200;https://x.com/a;1.5;True\nB;₴300,50;www.y.com;2e-3;False\n").unwrap();
        assert_eq!(t.headings, ["name", "price", "link", "n", "flag"]);
        let kinds: Vec<ColumnKind> = (0..5)
            .map(|j| infer_kind(&t.headings[j], &t.rows.iter().filter_map(|r| r[j].as_deref()).collect::<Vec<_>>()))
            .collect();
        assert_eq!(kinds[0], ColumnKind::Text);
        assert_eq!(kinds[1], ColumnKind::Money { currency: Currency::new("UAH").unwrap() });
        assert_eq!(kinds[2], ColumnKind::Link);
        assert_eq!(kinds[3], ColumnKind::Number);
        assert_eq!(kinds[4], ColumnKind::Text);
    }

    #[test]
    fn dates_and_times() {
        let k = |h: &str, v: &[&str]| infer_kind(h, v);
        assert_eq!(k("ts", &["1789728376.130244", "1789728377.659081"]), ColumnKind::DateTime);
        assert_eq!(k("rssi", &["1789728376", "1789728377"]), ColumnKind::Number);
        assert_eq!(k("created_at", &["1789728376130"]), ColumnKind::DateTime);
        assert_eq!(k("timestamp", &["2026-09-18T10:46:16.130244+00:00"]), ColumnKind::DateTime);
        assert_eq!(k("when", &["2026-09-18 10:46"]), ColumnKind::DateTime);
        assert_eq!(k("day", &["2026-09-18", "19.09.2026"]), ColumnKind::Date);
        assert_eq!(k("ts", &["12", "13"]), ColumnKind::Number);
    }

    #[test]
    fn json_records() {
        let t = read_json(r#"[{"a": 1, "b": "x"}, {"a": 2.5, "c": null, "b": true}]"#).unwrap();
        assert_eq!(t.headings, ["a", "b", "c"]);
        assert_eq!(t.rows[1], [Some("2.5".into()), Some("true".into()), None]);
    }
    #[test]
    fn excel_numbers() {
        assert_eq!(excel_number(0.1 + 0.2), "0.3");
        assert_eq!(excel_number(1200.0), "1200");
        assert_eq!(excel_number(77.85), "77.85");
        assert_eq!(excel_number(-0.000123), "-0.000123");
    }

    #[test]
    fn totals_rows() {
        assert!(is_totals_formula("SUM(F2:F9)"));
        assert!(is_totals_formula("=SUBTOTAL(9,$C$2:$C$20)"));
        assert!(is_totals_formula("SUM(Sheet1!B2:B9)"));
        assert!(!is_totals_formula("SUM(B2:D2)"));
        assert!(!is_totals_formula("E2*F2"));
    }

    #[test]
    fn excel_workbook() {
        use rust_xlsxwriter::{Format, Formula, Workbook};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("drone parts.xlsx");
        let mut wb = Workbook::new();
        let date = Format::new().set_num_format("yyyy-mm-dd");
        let ws = wb.add_worksheet().set_name("Parts").unwrap();
        ws.write(0, 0, "Drone parts, October").unwrap();
        for (j, h) in ["Item", "Link", "Bought", "USD", "Qty", "Total"].iter().enumerate() {
            ws.write(2, j as u16, *h).unwrap();
        }
        let rows = [("Motor", "https://x.com/m", 46000.0, 12.5, 4.0), ("Frame", "https://x.com/f", 46001.0, 0.1 + 0.2, 1.0)];
        for (i, (item, link, day, usd, qty)) in rows.iter().enumerate() {
            let r = 3 + i as u32;
            ws.write(r, 0, *item).unwrap();
            ws.write(r, 1, *link).unwrap();
            ws.write_with_format(r, 2, *day, &date).unwrap();
            ws.write(r, 3, *usd).unwrap();
            ws.write(r, 4, *qty).unwrap();
            ws.write_formula(r, 5, Formula::new(format!("=D{}*E{}", r + 1, r + 1)).set_result((usd * qty).to_string())).unwrap();
        }
        ws.write(5, 0, "Total").unwrap();
        ws.write_formula(5, 5, Formula::new("=SUM(F4:F5)").set_result("50.3")).unwrap();
        let notes = wb.add_worksheet().set_name("Notes").unwrap();
        notes.write(0, 0, "Note").unwrap();
        notes.write(1, 0, "ask about bulk price").unwrap();
        wb.add_worksheet().set_name("Empty").unwrap();
        wb.save(&path).unwrap();

        let sheets = read_tables(&path).unwrap();
        let names: Vec<&str> = sheets.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["Parts", "Notes"]);
        let t = &sheets[0].1;
        assert_eq!(t.headings, ["Item", "Link", "Bought", "USD", "Qty", "Total"]);
        assert_eq!(t.rows.len(), 2, "title and totals rows are left out");
        assert_eq!(t.rows[0][2].as_deref(), Some("2025-12-09"));
        assert_eq!(t.rows[1][3].as_deref(), Some("0.3"));
        assert_eq!(t.rows[0][5].as_deref(), Some("50"));
        let kinds: Vec<ColumnKind> = (0..6)
            .map(|j| infer_kind(&t.headings[j], &t.rows.iter().filter_map(|r| r[j].as_deref()).collect::<Vec<_>>()))
            .collect();
        assert_eq!(kinds, [ColumnKind::Text, ColumnKind::Link, ColumnKind::Date, ColumnKind::Number, ColumnKind::Number, ColumnKind::Number]);

        // A workbook with one worksheet takes the file's name.
        let one = dir.path().join("list.xlsx");
        let mut wb = Workbook::new();
        let ws = wb.add_worksheet();
        ws.write(0, 0, "Item").unwrap();
        ws.write(0, 1, "UAH").unwrap();
        ws.write(1, 0, "Cable").unwrap();
        ws.write(1, 1, 320).unwrap();
        wb.save(&one).unwrap();
        let sheets = read_tables(&one).unwrap();
        assert_eq!(sheets.len(), 1);
        assert_eq!(sheets[0].0, "list");
    }
}
