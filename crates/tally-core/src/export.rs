//! Export a cost list to XLSX. Totals are real `=SUM()` formulas, so the file keeps
//! working as a spreadsheet; links stay clickable.

use std::path::Path;

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_xlsxwriter::{Format, FormatBorder, Formula, Url, Workbook};
use std::str::FromStr;

use crate::costlist::CostListView;
use crate::money::Currency;
use crate::ops::RateMode;
use crate::Result;

fn number_format(c: &Currency) -> String {
    match c.minor_units() {
        0 => "#,##0".into(),
        n => format!("#,##0.{}", "0".repeat(n as usize)),
    }
}

fn num(s: &str) -> Option<f64> {
    Decimal::from_str(s).ok().and_then(|d| d.to_f64())
}

fn col_letter(mut col: u16) -> String {
    let mut s = String::new();
    col += 1;
    while col > 0 {
        let rem = (col - 1) % 26;
        s.insert(0, (b'A' + rem as u8) as char);
        col = (col - 1) / 26;
    }
    s
}

pub fn export_xlsx(view: &CostListView, path: &Path) -> Result<()> {
    let mut wb = Workbook::new();
    let ws = wb.add_worksheet();
    let sheet_name: String = view.name.chars().filter(|c| !"[]:*?/\\".contains(*c)).take(31).collect();
    if !sheet_name.trim().is_empty() {
        ws.set_name(sheet_name.trim())?;
    }

    let bold = Format::new().set_bold();
    let header = Format::new().set_bold().set_border_bottom(FormatBorder::Thin);
    let total_label = Format::new().set_bold().set_border_top(FormatBorder::Thin);
    let quiet = Format::new().set_font_color("#666666");

    let mut headers: Vec<String> =
        ["Item", "Link", "Qty", "Unit price", "Currency", "Line total"].iter().map(|s| s.to_string()).collect();
    for c in &view.currencies {
        headers.push(format!("Total {c}"));
    }
    for (i, h) in headers.iter().enumerate() {
        ws.write_string_with_format(0, i as u16, h, &header)?;
    }

    let first_data = 1u32;
    for (i, r) in view.rows.iter().enumerate() {
        let row = first_data + i as u32;
        if let Some(item) = &r.item {
            ws.write_string(row, 0, item)?;
        }
        if let Some(url) = &r.url {
            if url.starts_with("http://") || url.starts_with("https://") {
                ws.write_url(row, 1, Url::new(url.as_str()))?;
            } else {
                ws.write_string(row, 1, url)?;
            }
        }
        if let Some(q) = r.qty.as_deref().and_then(num) {
            ws.write_number(row, 2, q)?;
        }
        let row_cur = r.currency.as_deref().and_then(|c| Currency::new(c).ok());
        if let Some(p) = r.unit_price.as_deref().and_then(num) {
            ws.write_number(row, 3, p)?;
        }
        if let Some(c) = &r.currency {
            ws.write_string(row, 4, c)?;
        }
        if let (Some(t), Some(c)) = (r.line_total.as_deref().and_then(num), &row_cur) {
            ws.write_number_with_format(row, 5, t, &Format::new().set_num_format(number_format(c)))?;
        }
        for (j, c) in view.currencies.iter().enumerate() {
            if let Some(v) = r.converted[j].as_deref().and_then(num) {
                ws.write_number_with_format(row, 6 + j as u16, v, &Format::new().set_num_format(number_format(c)))?;
            }
        }
    }

    let total_row = first_data + view.rows.len() as u32;
    ws.write_string_with_format(total_row, 0, "Total", &total_label)?;
    for col in 1..6u16 {
        ws.write_blank(total_row, col, &total_label)?;
    }
    for (j, (c, t)) in view.currencies.iter().zip(&view.totals).enumerate() {
        let col = 6 + j as u16;
        let letter = col_letter(col);
        let formula = if view.rows.is_empty() {
            Formula::new("=0")
        } else {
            Formula::new(format!("=SUM({letter}{}:{letter}{})", first_data + 1, total_row))
        }
        .set_result(&t.amount);
        let fmt = Format::new().set_bold().set_border_top(FormatBorder::Thin).set_num_format(number_format(c));
        ws.write_formula_with_format(total_row, col, formula, &fmt)?;
    }

    let mut note_row = total_row + 2;
    let mode = match &view.rates {
        RateMode::Live => format!("Rates as of {} (live)", view.as_of),
        RateMode::Locked { date } => format!("Rates locked at {date}"),
    };
    ws.write_string_with_format(note_row, 0, &mode, &bold)?;
    for leg in &view.rates_used {
        note_row += 1;
        let text = format!("1 {} = {} {} ({}, {})", leg.from, leg.rate, leg.to, leg.source, leg.date);
        ws.write_string_with_format(note_row, 0, &text, &quiet)?;
    }
    for w in &view.warnings {
        note_row += 1;
        ws.write_string_with_format(note_row, 0, &format!("Note: {w}"), &quiet)?;
    }

    ws.set_column_width(0, 32)?;
    ws.set_column_width(1, 36)?;
    ws.set_column_width(2, 6)?;
    ws.set_column_width(3, 12)?;
    ws.set_column_width(4, 9)?;
    for col in 5..(6 + view.currencies.len() as u16) {
        ws.set_column_width(col, 14)?;
    }
    ws.set_freeze_panes(1, 0)?;

    wb.save(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_letters() {
        assert_eq!(col_letter(0), "A");
        assert_eq!(col_letter(6), "G");
        assert_eq!(col_letter(25), "Z");
        assert_eq!(col_letter(26), "AA");
    }
}
