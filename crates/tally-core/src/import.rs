//! Importing a file as a new sheet: CSV (any delimiter) or JSON. The file's
//! headings become the columns, and each column's type is worked out from its
//! values, so a file with its own layout comes in typed and ready to use.

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
    // The first line is headings unless it looks like data (all numbers).
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
    Ok(Table { headings, rows: rows.filter(|r| r.iter().any(|c| c.is_some())).collect() })
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

/// Read a file by its extension.
pub fn read_file(path: &std::path::Path) -> Result<Table> {
    let text = std::fs::read_to_string(path)?;
    match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("json") => read_json(&text),
        Some("csv" | "tsv" | "txt") | None => read_csv(&text),
        Some(other) => Err(Error::InvalidValue(format!("can't import .{other} files yet: use CSV or JSON"))),
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
}
