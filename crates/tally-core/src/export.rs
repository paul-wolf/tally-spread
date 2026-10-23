//! Export a sheet to XLSX. Totals and section subtotals are real `=SUM()`
//! formulas, so the file keeps working as a spreadsheet; links stay clickable.

use std::path::Path;
use std::str::FromStr;

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_xlsxwriter::{Color, Format, FormatBorder, Formula, Url, Workbook};

use crate::money::Currency;
use crate::ops::{ColumnKind, RateMode, RowKind};
use crate::sheet::{col_letter, SheetView};
use crate::Result;

fn number_format(c: Option<&Currency>) -> String {
    match c.map(|c| c.minor_units()) {
        Some(0) => "#,##0".into(),
        Some(n) => format!("#,##0.{}", "0".repeat(n as usize)),
        None => "#,##0.########".into(),
    }
}

fn num(s: &str) -> Option<f64> {
    Decimal::from_str(s).ok().and_then(|d| d.to_f64())
}

/// A stored date or moment as an Excel serial number (days since 1899-12-30, in
/// local time), and whether it has a time of day.
fn excel_time(v: &str) -> Option<(f64, bool)> {
    use chrono::{Local, NaiveDate, Timelike};
    let epoch = NaiveDate::from_ymd_opt(1899, 12, 30)?;
    if v.len() == 10 {
        let d = NaiveDate::parse_from_str(v, "%Y-%m-%d").ok()?;
        return Some(((d - epoch).num_days() as f64, false));
    }
    if !v.contains('T') {
        return None;
    }
    let t = chrono::DateTime::parse_from_rfc3339(v).ok()?.with_timezone(&Local).naive_local();
    let day = (t.date() - epoch).num_days() as f64;
    let secs = t.num_seconds_from_midnight() as f64 + t.nanosecond() as f64 / 1e9;
    Some((day + secs / 86_400.0, true))
}

pub fn export_xlsx(view: &SheetView, path: &Path) -> Result<()> {
    let mut wb = Workbook::new();
    let ws = wb.add_worksheet();
    let sheet_name: String = view.name.chars().filter(|c| !"[]:*?/\\".contains(*c)).take(31).collect();
    if !sheet_name.trim().is_empty() {
        ws.set_name(sheet_name.trim())?;
    }

    let header = Format::new().set_bold().set_border_bottom(FormatBorder::Thin);
    let section = Format::new().set_bold().set_background_color(Color::RGB(0xF0F0EC));
    let total_fmt = Format::new().set_bold().set_border_top(FormatBorder::Thin);
    let quiet = Format::new().set_font_color(Color::RGB(0x666666));

    for (j, c) in view.columns.iter().enumerate() {
        ws.write_string_with_format(0, j as u16, &c.name, &header)?;
        let width = match &c.kind {
            ColumnKind::Text => 30.0,
            ColumnKind::Link => 34.0,
            ColumnKind::Number => 8.0,
            _ => 14.0,
        };
        ws.set_column_width(j as u16, width)?;
    }

    // Which column currency to format each column with (from its first value).
    let col_currency: Vec<Option<Currency>> = (0..view.columns.len())
        .map(|j| match &view.columns[j].kind {
            ColumnKind::Money { currency } => Some(currency.clone()),
            _ => view.rows.iter().find_map(|r| r.cells[j].currency.clone()),
        })
        .collect();

    let first = 1u32;
    let last_row = first + view.rows.len() as u32; // exclusive
    for (i, r) in view.rows.iter().enumerate() {
        let row = first + i as u32;
        if r.kind == RowKind::Section {
            // Subtotal formulas over the rows up to the next section.
            let end = view.rows[i + 1..]
                .iter()
                .position(|x| x.kind == RowKind::Section)
                .map_or(last_row, |k| row + 1 + k as u32);
            for (j, c) in view.columns.iter().enumerate() {
                let col = j as u16;
                let fmt = section.clone().set_num_format(number_format(col_currency[j].as_ref()));
                if c.total && end > row + 1 {
                    let l = col_letter(j);
                    let mut f = Formula::new(format!("=SUM({l}{}:{l}{})", row + 2, end));
                    if let Some(v) = &r.cells[j].value {
                        f = f.set_result(v);
                    }
                    ws.write_formula_with_format(row, col, f, &fmt)?;
                } else if let Some(raw) = &r.cells[j].raw {
                    ws.write_string_with_format(row, col, raw, &section)?;
                } else {
                    ws.write_blank(row, col, &section)?;
                }
            }
            continue;
        }
        for (j, c) in view.columns.iter().enumerate() {
            let col = j as u16;
            let cell = &r.cells[j];
            let mut fmt = Format::new().set_num_format(number_format(col_currency[j].as_ref()));
            if c.style.bold {
                fmt = fmt.set_bold();
            }
            match (&c.kind, &cell.raw, &cell.value) {
                (ColumnKind::Link, Some(url), _) if url.starts_with("http://") || url.starts_with("https://") => {
                    ws.write_url(row, col, Url::new(url.as_str()))?;
                }
                (ColumnKind::Date | ColumnKind::DateTime, Some(v), _) | (ColumnKind::Calc { .. }, None, Some(v))
                    if excel_time(v).is_some() =>
                {
                    let (serial, with_time) = excel_time(v).unwrap();
                    let f = Format::new().set_num_format(if with_time { "yyyy-mm-dd hh:mm:ss" } else { "yyyy-mm-dd" });
                    let f = if c.style.bold { f.set_bold() } else { f };
                    ws.write_number_with_format(row, col, serial, &f)?;
                }
                (ColumnKind::Number | ColumnKind::Money { .. }, Some(v), _) | (ColumnKind::Calc { .. }, None, Some(v)) => {
                    match num(v) {
                        Some(n) => {
                            ws.write_number_with_format(row, col, n, &fmt)?;
                        }
                        None => {
                            ws.write_string_with_format(row, col, v, &fmt)?;
                        }
                    }
                }
                (_, Some(v), _) => {
                    ws.write_string_with_format(row, col, v, &fmt)?;
                }
                _ => {}
            }
        }
    }

    // Grand totals: SUM over item rows only, so subtotals are not counted twice.
    let total_row = last_row;
    let item_rows: Vec<u32> =
        view.rows.iter().enumerate().filter(|(_, r)| r.kind == RowKind::Item).map(|(i, _)| first + i as u32 + 1).collect();
    let has_sections = view.rows.iter().any(|r| r.kind == RowKind::Section);
    let mut wrote_label = false;
    for (j, c) in view.columns.iter().enumerate() {
        let col = j as u16;
        let Some(t) = &view.totals[j] else { continue };
        let l = col_letter(j);
        let expr = if view.rows.is_empty() {
            "=0".to_string()
        } else if has_sections {
            let refs: Vec<String> = item_rows.iter().map(|r| format!("{l}{r}")).collect();
            format!("=SUM({})", refs.join(","))
        } else {
            format!("=SUM({l}{}:{l}{})", first + 1, last_row)
        };
        let fmt = total_fmt.clone().set_num_format(number_format(col_currency[j].as_ref()));
        ws.write_formula_with_format(total_row, col, Formula::new(expr).set_result(&t.amount), &fmt)?;
        if j > 0 && !wrote_label && c.total {
            ws.write_string_with_format(total_row, 0, "Total", &total_fmt)?;
            wrote_label = true;
        }
    }

    let mut note = total_row + 2;
    let mode = match &view.rates {
        RateMode::Live => format!("Rates as of {} (live)", view.as_of),
        RateMode::Locked { date } => format!("Rates locked at {date}"),
    };
    ws.write_string_with_format(note, 0, &mode, &quiet)?;
    for leg in &view.rates_used {
        note += 1;
        ws.write_string_with_format(note, 0, &format!("1 {} = {} {} ({}, {})", leg.from, leg.rate, leg.to, leg.source, leg.date), &quiet)?;
    }
    for w in &view.warnings {
        note += 1;
        ws.write_string_with_format(note, 0, &format!("Note: {w}"), &quiet)?;
    }
    ws.set_freeze_panes(1, 0)?;
    wb.save(path)?;
    Ok(())
}
