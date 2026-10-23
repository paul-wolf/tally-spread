//! Starting layouts for new sheets. Every column can be changed afterwards.

use crate::money::Currency;
use crate::ops::{ColumnKind, ColumnSpec, Op};

pub const TEMPLATES: &[(&str, &str)] = &[
    ("blank", "Blank: five text columns"),
    ("spending", "Spending list: prices in UAH or USD, unit cost and total in USD"),
];

fn col(sheet_id: &str, position: usize, name: &str, kind: ColumnKind, total: bool) -> Op {
    Op::AddColumn {
        sheet_id: sheet_id.into(),
        col_id: String::new(), // next free ID: c1, c2, …
        position: position as f64,
        spec: ColumnSpec { name: name.into(), var: String::new(), kind, total, style: Default::default() },
    }
}

fn money(code: &str) -> ColumnKind {
    ColumnKind::Money { currency: Currency::new(code).expect("valid code") }
}

/// The operations that create a sheet from a template (one undo step when batched).
pub fn sheet_ops(template: &str, sheet_id: &str, name: &str) -> Vec<Op> {
    let mut ops = vec![Op::CreateSheet { sheet_id: sheet_id.into(), name: name.into() }];
    match template {
        "spending" => {
            let usd = || Some(Currency::new("USD").expect("valid"));
            let calc = |f: &str| ColumnKind::Calc { formula: f.into(), currency: usd() };
            ops.extend([
                col(sheet_id, 1, "Item", ColumnKind::Text, false),
                col(sheet_id, 2, "Link", ColumnKind::Link, false),
                col(sheet_id, 3, "Note", ColumnKind::Text, false),
                col(sheet_id, 4, "Price (UAH)", money("UAH"), false),
                col(sheet_id, 5, "Price (USD)", money("USD"), false),
                col(sheet_id, 6, "Unit (USD)", calc("coalesce(price_uah, price_usd)"), false),
                col(sheet_id, 7, "Qty", ColumnKind::Number, false),
                col(sheet_id, 8, "Total (USD)", calc("unit_usd * qty"), true),
                Op::SetSheetBase { sheet_id: sheet_id.into(), base: Some(Currency::new("USD").expect("valid")) },
            ]);
        }
        _ => {
            for i in 1..=5 {
                ops.push(col(sheet_id, i, &format!("Column {i}"), ColumnKind::Text, false));
            }
        }
    }
    ops
}
