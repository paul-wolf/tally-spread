//! Starting layouts for new sheets. Every column can be changed afterwards.

use crate::money::Currency;
use crate::ops::{ColumnKind, ColumnSpec, Op};

pub const TEMPLATES: &[(&str, &str)] = &[
    ("blank", "Blank: columns A–E, all text"),
    ("spending", "Spending list: UAH and USD prices, unit cost and total in USD"),
];

fn col(sheet_id: &str, position: usize, name: &str, kind: ColumnKind, total: bool) -> Op {
    Op::AddColumn {
        sheet_id: sheet_id.into(),
        col_id: Op::new_id(),
        position: position as f64,
        spec: ColumnSpec { name: name.into(), kind, total },
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
            let calc = |f: &str| ColumnKind::Calc { formula: f.into() };
            ops.extend([
                col(sheet_id, 1, "Item", ColumnKind::Text, false),
                col(sheet_id, 2, "Link", ColumnKind::Link, false),
                col(sheet_id, 3, "Note", ColumnKind::Text, false),
                col(sheet_id, 4, "UAH", money("UAH"), false),
                col(sheet_id, 5, "USD", money("USD"), false),
                col(sheet_id, 6, "Unit USD", calc("coalesce(to(UAH, USD), USD)"), false),
                col(sheet_id, 7, "Qty", ColumnKind::Number, false),
                col(sheet_id, 8, "Total USD", calc("[Unit USD] * Qty"), true),
            ]);
        }
        _ => {
            for (i, n) in ["A", "B", "C", "D", "E"].iter().enumerate() {
                ops.push(col(sheet_id, i + 1, n, ColumnKind::Text, false));
            }
        }
    }
    ops
}
