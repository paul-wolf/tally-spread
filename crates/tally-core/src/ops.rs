//! Operations: every change to a project is one of these. The UI, keyboard
//! shortcuts and the AI all produce operations; the project applies them, logs them
//! and can undo them.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::fx::RateSource;
use crate::money::Currency;
use crate::{Error, Result};

/// What a column holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ColumnKind {
    Text,
    /// A web address; shown as a link.
    Link,
    /// A plain number, e.g. a quantity.
    Number,
    /// Amounts in one currency, e.g. a "UAH" price column.
    Money { currency: Currency },
    /// Computed for every row from a formula, e.g. `coalesce(to(price_uah, USD), price_usd)`.
    /// Stored with column IDs (`coalesce(to(c4, USD), c5)`), so renames never break it.
    /// With a `currency`, results are amounts in that currency: other currencies are
    /// converted at the sheet's rates (so `coalesce(price_uah, price_usd)` in a USD
    /// column needs no `to()`), and plain numbers are taken as amounts in it.
    Calc {
        formula: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        currency: Option<Currency>,
    },
}

impl ColumnKind {
    pub fn is_input(&self) -> bool {
        !matches!(self, ColumnKind::Calc { .. })
    }
}

/// A column's definition. Besides its invariant ID (`c4`, see [`Op::AddColumn`]) a
/// column has a display name (`Price (UAH)`) and a variable name for formulas and
/// SQL (`price_uah`; derived from the display name when left empty). `total` adds
/// the column up per section and for the sheet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnSpec {
    /// Display name, shown in the header.
    pub name: String,
    /// Variable name: lowercase letters, digits and `_`.
    #[serde(default)]
    pub var: String,
    #[serde(flatten)]
    pub kind: ColumnKind,
    #[serde(default)]
    pub total: bool,
    /// How the column looks (format, not type).
    #[serde(default, skip_serializing_if = "Style::is_plain")]
    pub style: Style,
}

/// How a column is shown, on screen and in exports.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Style {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bold: bool,
}

impl Style {
    pub fn is_plain(&self) -> bool {
        *self == Style::default()
    }
}

/// Rows are items, or section headings that group the items below them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RowKind {
    #[default]
    Item,
    Section,
}

impl RowKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            RowKind::Item => "item",
            RowKind::Section => "section",
        }
    }

    pub fn parse(s: &str) -> Self {
        if s == "section" {
            RowKind::Section
        } else {
            RowKind::Item
        }
    }
}

/// Which day's exchange rates a sheet uses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum RateMode {
    /// Always the latest rates.
    #[default]
    Live,
    /// Rates in force on a fixed date, so totals never change.
    Locked { date: NaiveDate },
}

impl RateMode {
    pub fn to_db(&self) -> String {
        match self {
            RateMode::Live => "live".into(),
            RateMode::Locked { date } => format!("locked:{date}"),
        }
    }

    pub fn from_db(s: &str) -> Result<Self> {
        if s == "live" {
            return Ok(RateMode::Live);
        }
        s.strip_prefix("locked:")
            .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
            .map(|date| RateMode::Locked { date })
            .ok_or_else(|| Error::InvalidValue(format!("rate mode {s:?}")))
    }

    pub fn as_of(&self, today: NaiveDate) -> NaiveDate {
        match self {
            RateMode::Live => today,
            RateMode::Locked { date } => *date,
        }
    }
}

/// A rate set by hand for one sheet: `1 base = rate quote`. It wins over any source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixedRate {
    pub base: Currency,
    pub quote: Currency,
    /// Decimal as text, e.g. "44.92".
    pub rate: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    CreateSheet { sheet_id: String, name: String },
    RenameSheet { sheet_id: String, name: String },
    /// Archived sheets are hidden but kept, so archiving can be undone.
    SetSheetArchived { sheet_id: String, archived: bool },
    SetSheetRates { sheet_id: String, rates: RateMode },
    /// Rate sources in order of preference; empty means the project's.
    SetSheetSources { sheet_id: String, sources: Vec<RateSource> },
    /// The rate card's base currency (`None` = choose automatically).
    SetSheetBase { sheet_id: String, base: Option<Currency> },
    /// Hand-set rates for this sheet; they win over every source.
    SetSheetFixedRates { sheet_id: String, rates: Vec<FixedRate> },

    /// `position` orders columns; insert between two with the midpoint. Leave
    /// `col_id` empty to get the next free ID (`c1`, `c2`, …); IDs are never reused.
    AddColumn { sheet_id: String, col_id: String, position: f64, spec: ColumnSpec },
    /// Change a column's names, type or formula.
    UpdateColumn { sheet_id: String, col_id: String, spec: ColumnSpec },
    MoveColumn { sheet_id: String, col_id: String, position: f64 },
    /// Deleted columns are hidden but kept (with their values), so deleting can be undone.
    SetColumnDeleted { sheet_id: String, col_id: String, deleted: bool },

    /// `position` orders rows; insert between two rows with the midpoint.
    InsertRow { sheet_id: String, row_id: String, position: f64 },
    SetRowKind { sheet_id: String, row_id: String, kind: RowKind },
    /// Deleted rows are hidden but kept, so deleting can be undone.
    SetRowDeleted { sheet_id: String, row_id: String, deleted: bool },
    /// `None` clears the cell. Amounts are stored normalised (`1200.5`, not `1 200,50`).
    SetCell { sheet_id: String, row_id: String, col_id: String, value: Option<String> },

    /// Several operations applied, logged and undone as one.
    Batch { ops: Vec<Op> },
}

impl Op {
    pub fn new_id() -> String {
        Ulid::generate().to_string()
    }
}

/// One line of a project's operation log.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogEntry {
    /// Time-ordered unique id; entries are replayed in id order.
    pub id: Ulid,
    pub ts: DateTime<Utc>,
    pub machine: String,
    pub op: Op,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_json_is_readable() {
        let op = Op::AddColumn {
            sheet_id: "s".into(),
            col_id: "c".into(),
            position: 3.0,
            spec: ColumnSpec {
                name: "Price (UAH)".into(),
                var: "price_uah".into(),
                kind: ColumnKind::Money { currency: Currency::new("UAH").unwrap() },
                total: false,
                style: Default::default(),
            },
        };
        let json = serde_json::to_string(&op).unwrap();
        assert_eq!(
            json,
            r#"{"op":"add_column","sheet_id":"s","col_id":"c","position":3.0,"spec":{"name":"Price (UAH)","var":"price_uah","type":"money","currency":"UAH","total":false}}"#
        );
        assert_eq!(serde_json::from_str::<Op>(&json).unwrap(), op);
    }

    #[test]
    fn rate_mode_round_trip() {
        let m = RateMode::Locked { date: NaiveDate::from_ymd_opt(2026, 10, 1).unwrap() };
        assert_eq!(RateMode::from_db(&m.to_db()).unwrap(), m);
        assert_eq!(RateMode::from_db("live").unwrap(), RateMode::Live);
    }
}
