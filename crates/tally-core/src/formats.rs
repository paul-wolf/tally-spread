//! Exports besides XLSX: CSV and JSON for data tools, PDF for people.
//!
//! All of them skip blank rows. CSV and JSON are tidy data (one row per item,
//! with its section as a field); the PDF looks like the sheet, with section
//! subtotals and the Total row.

use std::str::FromStr;

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde_json::{json, Map, Value};

use crate::money::{format_amount, Currency};
use crate::ops::{ColumnKind, RowKind};
use crate::sheet::{CellView, ColumnView, RowView, SheetView};
use crate::Result;

/// The fixed currency of a column, if it has one.
fn column_currency(c: &ColumnView) -> Option<&Currency> {
    match &c.kind {
        ColumnKind::Money { currency } => Some(currency),
        ColumnKind::Calc { currency, .. } => currency.as_ref(),
        _ => None,
    }
}

fn is_numeric(c: &ColumnView) -> bool {
    matches!(c.kind, ColumnKind::Number | ColumnKind::Money { .. } | ColumnKind::Calc { .. })
}

/// The value of a cell as plain text: what was typed, or what was calculated.
fn plain(c: &ColumnView, cell: &CellView) -> Option<String> {
    if c.kind.is_input() {
        cell.raw.clone().filter(|s| !s.trim().is_empty())
    } else {
        cell.value.clone()
    }
}

fn blank(r: &RowView, cols: &[ColumnView]) -> bool {
    r.kind == RowKind::Item && cols.iter().zip(&r.cells).all(|(c, cell)| plain(c, cell).is_none() && cell.error.is_none())
}

/// A section heading's label: the first thing typed on its row.
fn section_label(r: &RowView) -> String {
    r.cells.iter().find_map(|c| c.raw.clone().filter(|s| !s.trim().is_empty())).unwrap_or_default()
}

/// `1234567.5` -> `1,234,567.5`.
fn grouped(s: &str) -> String {
    let (sign, rest) = s.strip_prefix('-').map_or(("", s), |r| ("-", r));
    let (int, frac) = rest.split_once('.').map_or((rest, None), |(i, f)| (i, Some(f)));
    let mut out = String::new();
    for (i, ch) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    match frac {
        Some(f) => format!("{sign}{out}.{f}"),
        None => format!("{sign}{out}"),
    }
}

/// A stored moment (`2026-09-18T10:46:16Z`) in local time, as people read it.
fn moment(v: &str) -> Option<String> {
    if v.len() < 20 || !v.contains('T') {
        return None;
    }
    chrono::DateTime::parse_from_rfc3339(v).ok().map(|t| crate::time::show_moment(&t.with_timezone(&chrono::Utc)))
}

/// How a cell reads on paper.
fn display(c: &ColumnView, cell: &CellView) -> String {
    let Some(v) = plain(c, cell) else {
        return if cell.error.is_some() { "!".into() } else { String::new() };
    };
    if let Some(t) = moment(&v) {
        return t;
    }
    if !is_numeric(c) {
        return match c.kind {
            ColumnKind::Link => v.trim_start_matches("https://").trim_start_matches("http://").trim_start_matches("www.").to_string(),
            _ => v,
        };
    }
    let cur = column_currency(c).or(cell.currency.as_ref());
    match (Decimal::from_str(&v), cur) {
        (Ok(d), Some(cur)) => grouped(&format_amount(d, cur)),
        (Ok(d), None) => grouped(&d.normalize().to_string()),
        _ => v,
    }
}

fn number(s: &str) -> Value {
    Decimal::from_str(s)
        .ok()
        .and_then(|d| d.to_f64())
        .and_then(serde_json::Number::from_f64)
        .map_or_else(|| Value::String(s.to_string()), Value::Number)
}

// ------------------------------------------------------------------ CSV

/// The sheet as CSV: a header of display names, then one line per item. A
/// `Section` column is added when the sheet has sections.
pub fn to_csv(view: &SheetView) -> Result<String> {
    let has_sections = view.rows.iter().any(|r| r.kind == RowKind::Section);
    let mut w = csv::Writer::from_writer(Vec::new());
    let mut header: Vec<String> = Vec::new();
    if has_sections {
        header.push("Section".into());
    }
    header.extend(view.columns.iter().map(|c| c.name.clone()));
    w.write_record(&header).map_err(|e| crate::Error::parse("csv", e))?;
    let mut section = String::new();
    for r in &view.rows {
        if r.kind == RowKind::Section {
            section = section_label(r);
            continue;
        }
        if blank(r, &view.columns) {
            continue;
        }
        let mut rec: Vec<String> = Vec::new();
        if has_sections {
            rec.push(section.clone());
        }
        for (c, cell) in view.columns.iter().zip(&r.cells) {
            let v = plain(c, cell).unwrap_or_default();
            // A calculated column without a fixed currency says which one each value is in.
            let v = match (&c.kind, &cell.currency) {
                (ColumnKind::Calc { currency: None, .. }, Some(cur)) if !v.is_empty() => format!("{v} {cur}"),
                _ => v,
            };
            rec.push(v);
        }
        w.write_record(&rec).map_err(|e| crate::Error::parse("csv", e))?;
    }
    let bytes = w.into_inner().map_err(|e| crate::Error::parse("csv", e))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

// ------------------------------------------------------------------ JSON

/// The sheet as JSON: its columns, rates, items (values by variable name) and totals.
pub fn to_json(view: &SheetView, project: &str) -> Value {
    let columns: Vec<Value> = view
        .columns
        .iter()
        .map(|c| {
            let ty = serde_json::to_value(&c.kind).ok().and_then(|v| v.get("type").cloned()).unwrap_or(Value::Null);
            let mut o = json!({ "id": c.col_id, "name": c.name, "var": c.var, "type": ty });
            if let Some(cur) = column_currency(c) {
                o["currency"] = json!(cur.code());
            }
            if let Some(f) = &c.formula_display {
                o["formula"] = json!(f);
            }
            o
        })
        .collect();
    let mut rows = Vec::new();
    let mut section: Option<String> = None;
    for (n, r) in view.rows.iter().enumerate() {
        if r.kind == RowKind::Section {
            section = Some(section_label(r));
            continue;
        }
        if blank(r, &view.columns) {
            continue;
        }
        let mut values = Map::new();
        for (c, cell) in view.columns.iter().zip(&r.cells) {
            let v = match plain(c, cell) {
                None => Value::Null,
                Some(s) if is_numeric(c) => match (&c.kind, &cell.currency) {
                    (ColumnKind::Calc { currency: None, .. }, Some(cur)) => json!({ "amount": number(&s), "currency": cur.code() }),
                    _ => number(&s),
                },
                Some(s) => Value::String(s),
            };
            values.insert(c.var.clone(), v);
        }
        rows.push(json!({ "row": n + 1, "section": section, "values": values }));
    }
    let mut totals = Map::new();
    for (c, t) in view.columns.iter().zip(&view.totals) {
        if let Some(t) = t {
            totals.insert(
                c.var.clone(),
                json!({ "amount": number(&t.amount), "currency": t.currency.as_ref().map(|c| c.code()), "complete": t.complete }),
            );
        }
    }
    json!({
        "project": project,
        "sheet": view.name,
        "as_of": view.as_of.to_string(),
        "base": view.rate_card.base.code(),
        "rates": view.rates_used.iter().map(|l| json!({
            "from": l.from.code(), "to": l.to.code(), "rate": l.rate.to_string(),
            "source": l.source.label(), "date": l.date.to_string(),
        })).collect::<Vec<_>>(),
        "columns": columns,
        "rows": rows,
        "totals": totals,
    })
}

// ------------------------------------------------------------------ PDF

/// What the PDF template gets: display text only, laid out like the sheet.
fn pdf_data(view: &SheetView, project: &str) -> Value {
    let mut rows: Vec<Value> = Vec::new();
    let mut kinds: Vec<&str> = Vec::new();
    let cell = |t: String, b: bool, link: Option<String>| json!({ "t": t, "b": b, "link": link });
    for r in &view.rows {
        if blank(r, &view.columns) {
            continue;
        }
        let section = r.kind == RowKind::Section;
        let label = if section { section_label(r) } else { String::new() };
        let cells: Vec<Value> = view
            .columns
            .iter()
            .zip(&r.cells)
            .enumerate()
            .map(|(j, (c, cv))| {
                if section {
                    // The heading text in the first column, subtotals under their columns.
                    let t = if j == 0 {
                        label.clone()
                    } else if let Some(v) = &cv.value {
                        let cur = cv.currency.as_ref().or(column_currency(c));
                        Decimal::from_str(v).ok().map_or(v.clone(), |d| match cur {
                            Some(cur) => grouped(&format_amount(d, cur)),
                            None => grouped(&d.normalize().to_string()),
                        })
                    } else {
                        String::new()
                    };
                    return cell(t, true, None);
                }
                let link = match (&c.kind, &cv.raw) {
                    (ColumnKind::Link, Some(u)) if !u.is_empty() => {
                        Some(if u.contains("://") { u.clone() } else { format!("https://{u}") })
                    }
                    _ => None,
                };
                cell(display(c, cv), c.style.bold, link)
            })
            .collect();
        rows.push(Value::Array(cells));
        kinds.push(if section { "section" } else { "item" });
    }
    if view.totals.iter().any(|t| t.is_some()) {
        let cells: Vec<Value> = view
            .columns
            .iter()
            .zip(&view.totals)
            .enumerate()
            .map(|(j, (c, t))| {
                let t = match t {
                    Some(t) => {
                        let cur = t.currency.as_ref().or(column_currency(c));
                        let s = Decimal::from_str(&t.amount).ok().map_or(t.amount.clone(), |d| match cur {
                            Some(cur) => grouped(&format_amount(d, cur)),
                            None => grouped(&d.normalize().to_string()),
                        });
                        if t.complete { s } else { format!("{s} (incomplete)") }
                    }
                    None if j == 0 => "Total".into(),
                    None => String::new(),
                };
                cell(t, true, None)
            })
            .collect();
        rows.push(Value::Array(cells));
        kinds.push("total");
    }
    let columns: Vec<Value> = view
        .columns
        .iter()
        .map(|c| {
            let name = match column_currency(c) {
                Some(cur) if !c.name.contains(cur.code()) => format!("{} ({cur})", c.name),
                _ => c.name.clone(),
            };
            json!({ "name": name, "numeric": is_numeric(c), "weight": if matches!(c.kind, ColumnKind::Link) { 1.2 } else { 2.0 } })
        })
        .collect();
    let rates: Vec<String> = view
        .rates_used
        .iter()
        .map(|l| format!("1 {} = {} {} ({} {})", l.from, l.rate.normalize(), l.to, l.source.label(), l.date))
        .collect();
    let mode = match view.rates {
        crate::ops::RateMode::Live => "latest rates",
        crate::ops::RateMode::Locked { .. } => "rates pegged to",
    };
    let mut notes = Vec::new();
    if !rates.is_empty() {
        notes.push(format!("Rates: {}", rates.join(" · ")));
    }
    json!({
        "title": view.name,
        "subtitle": format!("{project} · {} as of {}", mode, view.as_of),
        "footer": format!("{project} · {}", view.name),
        "landscape": view.columns.len() > 6,
        "columns": columns,
        "rows": rows,
        "kinds": kinds,
        "notes": notes,
    })
}

#[cfg(feature = "pdf")]
pub fn to_pdf(view: &SheetView, project: &str) -> Result<Vec<u8>> {
    use typst::foundations::{Dict, IntoValue};
    use typst_as_lib::{typst_kit_options::TypstKitFontOptions, TypstEngine};

    static TEMPLATE: &str = include_str!("pdf.typ");
    let engine = TypstEngine::builder()
        .main_file(TEMPLATE)
        .search_fonts_with(TypstKitFontOptions::default().include_system_fonts(true).include_embedded_fonts(true))
        .build();
    let mut inputs = Dict::new();
    inputs.insert("data".into(), pdf_data(view, project).to_string().into_value());
    let doc = engine.compile_with_input(inputs).output.map_err(|e| crate::Error::parse("pdf", format!("{e:?}")))?;
    typst_pdf::pdf(&doc, &Default::default()).map_err(|e| crate::Error::parse("pdf", format!("{e:?}")))
}

#[cfg(test)]
mod tests {
    use super::grouped;

    #[test]
    fn groups_thousands() {
        assert_eq!(grouped("1234567.50"), "1,234,567.50");
        assert_eq!(grouped("-950"), "-950");
        assert_eq!(grouped("100"), "100");
        assert_eq!(grouped("1000"), "1,000");
    }
}
