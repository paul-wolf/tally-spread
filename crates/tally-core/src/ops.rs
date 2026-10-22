//! Operations: every change to a project is one of these. The UI, keyboard
//! shortcuts and the AI all produce operations; the project applies them, logs them
//! and can undo them.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::money::Currency;
use crate::{Error, Result};

/// An editable column of a cost list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Column {
    Item,
    Url,
    Qty,
    UnitPrice,
    Currency,
    Note,
}

impl Column {
    pub const ALL: [Column; 6] =
        [Column::Item, Column::Url, Column::Qty, Column::UnitPrice, Column::Currency, Column::Note];

    pub fn sql_name(&self) -> &'static str {
        match self {
            Column::Item => "item",
            Column::Url => "url",
            Column::Qty => "qty",
            Column::UnitPrice => "unit_price",
            Column::Currency => "currency",
            Column::Note => "note",
        }
    }

    pub fn is_decimal(&self) -> bool {
        matches!(self, Column::Qty | Column::UnitPrice)
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    CreateSheet { sheet_id: String, name: String },
    RenameSheet { sheet_id: String, name: String },
    /// Archived sheets are hidden but kept, so archiving can be undone.
    SetSheetArchived { sheet_id: String, archived: bool },
    /// Display currencies of a sheet; empty means the project default.
    SetSheetCurrencies { sheet_id: String, currencies: Vec<Currency> },
    SetSheetRates { sheet_id: String, rates: RateMode },
    /// `position` orders rows; insert between two rows with the midpoint.
    InsertRow { sheet_id: String, row_id: String, position: f64 },
    /// Deleted rows are hidden but kept, so deleting can be undone.
    SetRowDeleted { sheet_id: String, row_id: String, deleted: bool },
    /// `None` clears the cell. Amounts are stored normalised (`1200.5`, not `1 200,50`).
    SetCell { sheet_id: String, row_id: String, column: Column, value: Option<String> },
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
        let op = Op::SetCell {
            sheet_id: "s".into(),
            row_id: "r".into(),
            column: Column::UnitPrice,
            value: Some("12.5".into()),
        };
        let json = serde_json::to_string(&op).unwrap();
        assert_eq!(
            json,
            r#"{"op":"set_cell","sheet_id":"s","row_id":"r","column":"unit_price","value":"12.5"}"#
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
