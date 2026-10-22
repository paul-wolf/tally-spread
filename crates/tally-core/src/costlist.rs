//! Cost lists: items priced in any currency, shown with converted amounts and
//! totals in one or more display currencies.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Serialize;

use crate::fx::{RateLeg, RateTable};
use crate::money::{format_amount, round, Currency, RoundingScope};
use crate::ops::RateMode;
use crate::project::Project;
use crate::Result;

/// Rates older than this (relative to the sheet's date) are flagged.
pub const STALE_AFTER_DAYS: i64 = 7;

#[derive(Clone, Debug, Serialize)]
pub struct CostListView {
    pub sheet_id: String,
    pub name: String,
    pub currencies: Vec<Currency>,
    pub rates: RateMode,
    /// The date whose rates are used.
    pub as_of: NaiveDate,
    pub rows: Vec<RowView>,
    /// One per display currency, in the same order.
    pub totals: Vec<TotalView>,
    /// Totals in each original currency, before conversion.
    pub subtotals: Vec<TotalView>,
    /// Rates used, as published: `1 from = rate to`.
    pub rates_used: Vec<RateLeg>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RowView {
    pub row_id: String,
    /// Sort key; a new row goes after the largest.
    pub position: f64,
    pub item: Option<String>,
    pub url: Option<String>,
    pub qty: Option<String>,
    pub unit_price: Option<String>,
    pub currency: Option<String>,
    pub note: Option<String>,
    /// qty × unit price in the row's own currency, rounded.
    pub line_total: Option<String>,
    /// Line total in each display currency, rounded; `None` if it could not be converted.
    pub converted: Vec<Option<String>>,
    pub problem: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TotalView {
    pub currency: Currency,
    pub amount: String,
    /// False when some priced rows could not be included.
    pub complete: bool,
}

impl Project {
    pub fn cost_list(&self, sheet_id: &str, rates: &RateTable, today: NaiveDate) -> Result<CostListView> {
        let sheet = self.sheet(sheet_id)?;
        let currencies = self.display_currencies(&sheet);
        let as_of = sheet.rates.as_of(today);
        let settings = self.settings();
        let rounding = settings.rounding;

        let mut rows = Vec::new();
        let mut warnings = Vec::new();
        let mut legs: Vec<RateLeg> = Vec::new();
        // Per display currency: (sum of rounded lines, sum of unrounded lines, complete)
        let mut sums: Vec<(Decimal, Decimal, bool)> = vec![(Decimal::ZERO, Decimal::ZERO, true); currencies.len()];
        let mut subtotals: BTreeMap<Currency, Decimal> = BTreeMap::new();

        for (i, r) in self.rows(sheet_id)?.into_iter().enumerate() {
            let label = r.item.clone().unwrap_or_else(|| format!("row {}", i + 1));
            let mut view = RowView {
                row_id: r.row_id.clone(),
                position: r.position,
                item: r.item.clone(),
                url: r.url.clone(),
                qty: r.qty.map(|d| d.to_string()),
                unit_price: r.unit_price.map(|d| d.to_string()),
                currency: r.currency.as_ref().map(|c| c.code().to_string()),
                note: r.note.clone(),
                line_total: None,
                converted: vec![None; currencies.len()],
                problem: None,
            };

            if let Some(price) = r.unit_price {
                match &r.currency {
                    None => {
                        view.problem = Some("no currency".into());
                        warnings.push(format!("{label}: price has no currency"));
                        for s in sums.iter_mut() {
                            s.2 = false;
                        }
                    }
                    Some(cur) => {
                        let line = round(r.qty.unwrap_or(Decimal::ONE) * price, cur, rounding);
                        view.line_total = Some(format_amount(line, cur));
                        *subtotals.entry(cur.clone()).or_default() += line;

                        for (j, target) in currencies.iter().enumerate() {
                            match rates.convert(line, cur, target, as_of, &settings.rate_sources) {
                                Ok(conv) => {
                                    let rounded = round(conv.amount, target, rounding);
                                    view.converted[j] = Some(format_amount(rounded, target));
                                    sums[j].0 += rounded;
                                    sums[j].1 += conv.amount;
                                    for leg in conv.legs.into_iter().map(published) {
                                        if !legs.contains(&leg) {
                                            legs.push(leg);
                                        }
                                    }
                                }
                                Err(e) => {
                                    sums[j].2 = false;
                                    view.problem.get_or_insert_with(|| e.to_string());
                                    let msg = e.to_string();
                                    if !warnings.contains(&msg) {
                                        warnings.push(msg);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            rows.push(view);
        }

        let totals = currencies
            .iter()
            .zip(&sums)
            .map(|(c, (rounded, raw, complete))| {
                let amount = match settings.rounding_scope {
                    RoundingScope::PerLine => *rounded,
                    RoundingScope::OnTotal => round(*raw, c, rounding),
                };
                TotalView { currency: c.clone(), amount: format_amount(amount, c), complete: *complete }
            })
            .collect();
        let subtotals = subtotals
            .into_iter()
            .map(|(c, a)| TotalView { amount: format_amount(a, &c), currency: c, complete: true })
            .collect();

        legs.sort_by(|a, b| a.source.cmp(&b.source).then(a.from.cmp(&b.from)).then(a.to.cmp(&b.to)));
        let stale: Vec<String> = legs
            .iter()
            // Monthly rates carry their own validity period, checked during conversion.
            .filter(|l| l.source != crate::fx::RateSource::HmrcMonthly)
            .filter(|l| (as_of - l.date).num_days() > STALE_AFTER_DAYS)
            .map(|l| format!("{} {}→{} from {}", l.source, l.from, l.to, l.date))
            .collect();
        if !stale.is_empty() {
            warnings.push(format!("rates older than {STALE_AFTER_DAYS} days: {}", stale.join(", ")));
        }

        Ok(CostListView {
            sheet_id: sheet.sheet_id,
            name: sheet.name,
            currencies,
            rates: sheet.rates,
            as_of,
            rows,
            totals,
            subtotals,
            rates_used: legs,
            warnings,
        })
    }
}

/// The leg as the source publishes it (`1 base = rate quote`), so a rate used in
/// both directions is listed once.
fn published(leg: RateLeg) -> RateLeg {
    if leg.inverted {
        RateLeg { from: leg.to, to: leg.from, inverted: false, ..leg }
    } else {
        leg
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::{RateRecord, RateSource};
    use crate::ops::{Column, Op};
    use crate::project::ProjectSettings;
    use rust_decimal_macros::dec;

    fn c(s: &str) -> Currency {
        Currency::new(s).unwrap()
    }
    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn rates() -> RateTable {
        let mut t = RateTable::new();
        let month = |base: &str, quote: &str, rate| RateRecord {
            date: d("2026-10-01"),
            valid_to: Some(d("2026-10-31")),
            base: c(base),
            quote: c(quote),
            rate,
            source: RateSource::HmrcMonthly,
        };
        t.add([month("GBP", "UAH", dec!(55.0)), month("GBP", "USD", dec!(1.25)), month("GBP", "EUR", dec!(1.15))]);
        t
    }

    #[test]
    fn three_currencies_into_two() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = ProjectSettings::new("Test", c("GBP"));
        s.display_currencies = vec![c("GBP"), c("UAH")];
        s.rate_sources = vec![RateSource::HmrcMonthly];
        let mut p = Project::create(tmp.path(), s, "mac").unwrap();
        p.apply(Op::CreateSheet { sheet_id: "s".into(), name: "Kit".into() }).unwrap();
        let rows = [("a", "Cable", "2", "₴550"), ("b", "Lamp", "1", "$25"), ("c", "Chair", "1", "£100")];
        for (i, (id, item, qty, price)) in rows.iter().enumerate() {
            p.apply(Op::InsertRow { sheet_id: "s".into(), row_id: id.to_string(), position: i as f64 }).unwrap();
            for (col, v) in [(Column::Item, *item), (Column::Qty, *qty), (Column::UnitPrice, *price)] {
                p.apply(Op::SetCell { sheet_id: "s".into(), row_id: id.to_string(), column: col, value: Some(v.into()) })
                    .unwrap();
            }
        }

        let v = p.cost_list("s", &rates(), d("2026-10-22")).unwrap();
        assert!(v.warnings.is_empty(), "{:?}", v.warnings);
        // 1100 UAH = 20 GBP; 25 USD = 20 GBP; 100 GBP.
        assert_eq!(v.rows[0].line_total.as_deref(), Some("1100.00"));
        assert_eq!(v.rows[0].converted, vec![Some("20.00".into()), Some("1100.00".into())]);
        assert_eq!(v.rows[1].converted, vec![Some("20.00".into()), Some("1100.00".into())]);
        assert_eq!(v.totals[0].amount, "140.00");
        assert_eq!(v.totals[1].amount, "7700.00");
        assert!(v.totals.iter().all(|t| t.complete));
        assert_eq!(v.subtotals.len(), 3);
        assert_eq!(v.rates_used.len(), 2);
    }

    #[test]
    fn missing_currency_and_rates_are_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = ProjectSettings::new("Test", c("GBP"));
        s.rate_sources = vec![RateSource::HmrcMonthly];
        let mut p = Project::create(tmp.path(), s, "mac").unwrap();
        p.apply(Op::CreateSheet { sheet_id: "s".into(), name: "Kit".into() }).unwrap();
        p.apply(Op::InsertRow { sheet_id: "s".into(), row_id: "a".into(), position: 1.0 }).unwrap();
        p.apply(Op::InsertRow { sheet_id: "s".into(), row_id: "b".into(), position: 2.0 }).unwrap();
        p.apply(Op::SetCell { sheet_id: "s".into(), row_id: "a".into(), column: Column::UnitPrice, value: Some("10".into()) })
            .unwrap();
        p.apply(Op::SetCell { sheet_id: "s".into(), row_id: "b".into(), column: Column::UnitPrice, value: Some("10 PLN".into()) })
            .unwrap();
        let v = p.cost_list("s", &rates(), d("2026-10-22")).unwrap();
        assert_eq!(v.rows[0].problem.as_deref(), Some("no currency"));
        assert!(v.rows[1].problem.as_deref().unwrap().contains("PLN→GBP"));
        assert!(!v.totals[0].complete);
    }
}
