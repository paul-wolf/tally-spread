//! Exchange rates: dated records from named sources, and exact conversion between
//! any two currencies through the rates available on a given date.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fmt;

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::money::Currency;
use crate::{Error, Result};

pub mod sources;

/// Where a rate came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RateSource {
    /// European Central Bank daily reference rates (EUR base).
    Ecb,
    /// National Bank of Ukraine official daily rates (UAH quote).
    Nbu,
    /// HMRC monthly exchange rates for customs and VAT (GBP base).
    HmrcMonthly,
    /// Entered by hand.
    Manual,
}

impl RateSource {
    pub fn label(&self) -> &'static str {
        match self {
            RateSource::Ecb => "ECB",
            RateSource::Nbu => "NBU",
            RateSource::HmrcMonthly => "HMRC monthly",
            RateSource::Manual => "Manual",
        }
    }
}

impl fmt::Display for RateSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// `1 base = rate quote`, valid from `date` (until `valid_to`, if the source says so).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RateRecord {
    pub date: NaiveDate,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_to: Option<NaiveDate>,
    pub base: Currency,
    pub quote: Currency,
    pub rate: Decimal,
    pub source: RateSource,
}

/// One step of a conversion from `from` to `to`, using a published rate
/// (`1 base = rate quote`). When `inverted`, the published pair runs the other way
/// and the amount is divided by `rate` instead of multiplied.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RateLeg {
    pub from: Currency,
    pub to: Currency,
    pub rate: Decimal,
    pub inverted: bool,
    pub source: RateSource,
    pub date: NaiveDate,
}

impl RateLeg {
    /// Units of `to` per unit of `from`.
    pub fn effective_rate(&self) -> Decimal {
        if self.inverted {
            Decimal::ONE / self.rate
        } else {
            self.rate
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Conversion {
    /// Unrounded result.
    pub amount: Decimal,
    pub legs: Vec<RateLeg>,
}

#[derive(Clone, Debug, Default)]
pub struct RateTable {
    // (source, base, quote) -> date -> record
    series: HashMap<(RateSource, Currency, Currency), BTreeMap<NaiveDate, RateRecord>>,
}

impl RateTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add records; a record for the same source, pair and date replaces the old one.
    pub fn add(&mut self, records: impl IntoIterator<Item = RateRecord>) {
        for r in records {
            if r.rate <= Decimal::ZERO || r.base == r.quote {
                continue;
            }
            self.series
                .entry((r.source, r.base.clone(), r.quote.clone()))
                .or_default()
                .insert(r.date, r);
        }
    }

    pub fn len(&self) -> usize {
        self.series.values().map(|s| s.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn records(&self) -> impl Iterator<Item = &RateRecord> {
        self.series.values().flat_map(|s| s.values())
    }

    /// Latest date with any rate from `source`.
    pub fn latest_date(&self, source: RateSource) -> Option<NaiveDate> {
        self.series
            .iter()
            .filter(|((s, _, _), _)| *s == source)
            .filter_map(|(_, dates)| dates.keys().next_back().copied())
            .max()
    }

    /// The rate in force on `as_of` for each pair, from the given sources.
    fn edges(&self, as_of: NaiveDate, sources: &[RateSource]) -> Vec<RateRecord> {
        let mut out = Vec::new();
        for ((source, _, _), dates) in &self.series {
            if !sources.contains(source) {
                continue;
            }
            if let Some((_, rec)) = dates.range(..=as_of).next_back() {
                if rec.valid_to.map_or(true, |to| as_of <= to) {
                    out.push(rec.clone());
                }
            }
        }
        out
    }

    /// Convert `amount` from one currency to another using rates in force on `as_of`.
    ///
    /// Sources are tried in the order given: a path using only the first source wins,
    /// then only the second, and so on; only if no single source can do it are
    /// sources combined. Within that, the path with the fewest steps wins.
    pub fn convert(
        &self,
        amount: Decimal,
        from: &Currency,
        to: &Currency,
        as_of: NaiveDate,
        sources: &[RateSource],
    ) -> Result<Conversion> {
        if from == to {
            return Ok(Conversion { amount, legs: vec![] });
        }
        for s in sources {
            if let Some(legs) = shortest_path(&self.edges(as_of, &[*s]), from, to) {
                return Ok(apply_legs(amount, legs));
            }
        }
        if sources.len() > 1 {
            if let Some(legs) = shortest_path(&self.edges(as_of, sources), from, to) {
                return Ok(apply_legs(amount, legs));
            }
        }
        let names: Vec<_> = sources.iter().map(|s| s.label()).collect();
        Err(Error::NotFound(format!(
            "no {from}→{to} rate on or before {as_of} from {}",
            if names.is_empty() { "any source".to_string() } else { names.join(", ") }
        )))
    }
}

fn apply_legs(amount: Decimal, legs: Vec<RateLeg>) -> Conversion {
    // Multiply first, divide once at the end, so a round trip through an
    // inverted rate stays exact wherever the arithmetic allows.
    let (num, den) = legs.iter().fold((amount, Decimal::ONE), |(n, d), leg| {
        if leg.inverted {
            (n, d * leg.rate)
        } else {
            (n * leg.rate, d)
        }
    });
    Conversion { amount: num / den, legs }
}

const MAX_LEGS: usize = 3;

fn shortest_path(edges: &[RateRecord], from: &Currency, to: &Currency) -> Option<Vec<RateLeg>> {
    // Adjacency: every record works in both directions.
    let mut adj: HashMap<&Currency, Vec<RateLeg>> = HashMap::new();
    for r in edges {
        adj.entry(&r.base).or_default().push(RateLeg {
            from: r.base.clone(),
            to: r.quote.clone(),
            rate: r.rate,
            inverted: false,
            source: r.source,
            date: r.date,
        });
        adj.entry(&r.quote).or_default().push(RateLeg {
            from: r.quote.clone(),
            to: r.base.clone(),
            rate: r.rate,
            inverted: true,
            source: r.source,
            date: r.date,
        });
    }
    // Deterministic order: prefer most recent rates, then by currency code.
    for legs in adj.values_mut() {
        legs.sort_by(|a, b| b.date.cmp(&a.date).then(a.to.cmp(&b.to)).then(a.source.cmp(&b.source)));
    }

    let mut seen: HashSet<Currency> = HashSet::from([from.clone()]);
    let mut queue: VecDeque<(Currency, Vec<RateLeg>)> = VecDeque::from([(from.clone(), vec![])]);
    while let Some((cur, path)) = queue.pop_front() {
        if path.len() >= MAX_LEGS {
            continue;
        }
        for leg in adj.get(&cur).into_iter().flatten() {
            if seen.contains(&leg.to) {
                continue;
            }
            let mut next = path.clone();
            next.push(leg.clone());
            if &leg.to == to {
                return Some(next);
            }
            seen.insert(leg.to.clone());
            queue.push_back((leg.to.clone(), next));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn c(s: &str) -> Currency {
        Currency::new(s).unwrap()
    }
    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }
    fn rec(date: &str, base: &str, quote: &str, rate: Decimal, source: RateSource) -> RateRecord {
        RateRecord { date: d(date), valid_to: None, base: c(base), quote: c(quote), rate, source }
    }

    fn table() -> RateTable {
        let mut t = RateTable::new();
        t.add([
            rec("2026-10-20", "EUR", "USD", dec!(1.10), RateSource::Ecb),
            rec("2026-10-20", "EUR", "GBP", dec!(0.85), RateSource::Ecb),
            rec("2026-10-21", "EUR", "USD", dec!(1.20), RateSource::Ecb),
            rec("2026-10-21", "USD", "UAH", dec!(41.5), RateSource::Nbu),
            rec("2026-10-21", "EUR", "UAH", dec!(48.0), RateSource::Nbu),
            rec("2026-10-21", "GBP", "UAH", dec!(55.0), RateSource::Nbu),
        ]);
        t
    }

    #[test]
    fn identity() {
        let t = RateTable::new();
        let r = t.convert(dec!(5), &c("GBP"), &c("GBP"), d("2026-10-21"), &[]).unwrap();
        assert_eq!(r.amount, dec!(5));
        assert!(r.legs.is_empty());
    }

    #[test]
    fn uses_rate_in_force_on_date() {
        let t = table();
        let a = t.convert(dec!(10), &c("EUR"), &c("USD"), d("2026-10-20"), &[RateSource::Ecb]).unwrap();
        assert_eq!(a.amount, dec!(11.00));
        let b = t.convert(dec!(10), &c("EUR"), &c("USD"), d("2026-10-25"), &[RateSource::Ecb]).unwrap();
        assert_eq!(b.amount, dec!(12.00));
        assert!(t.convert(dec!(10), &c("EUR"), &c("USD"), d("2026-10-19"), &[RateSource::Ecb]).is_err());
    }

    #[test]
    fn inverse_and_cross_rates_within_one_source() {
        let t = table();
        // UAH -> GBP through NBU directly (inverse of GBP/UAH).
        let r = t.convert(dec!(1100), &c("UAH"), &c("GBP"), d("2026-10-21"), &[RateSource::Nbu]).unwrap();
        assert_eq!(r.amount, dec!(20));
        assert_eq!(r.legs.len(), 1);
        // USD -> GBP through UAH, NBU only: 41.5 / 55.
        let r = t.convert(dec!(55), &c("USD"), &c("GBP"), d("2026-10-21"), &[RateSource::Nbu]).unwrap();
        assert_eq!(r.amount, dec!(41.5));
        assert!(r.legs.iter().all(|l| l.source == RateSource::Nbu));
    }

    #[test]
    fn prefers_first_source_then_mixes() {
        let t = table();
        let r = t
            .convert(dec!(1), &c("EUR"), &c("UAH"), d("2026-10-21"), &[RateSource::Ecb, RateSource::Nbu])
            .unwrap();
        // ECB alone cannot reach UAH, so NBU alone is used (single source beats mixing).
        assert!(r.legs.iter().all(|l| l.source == RateSource::Nbu));
        assert_eq!(r.amount, dec!(48.0));
    }

    #[test]
    fn respects_valid_to() {
        let mut t = RateTable::new();
        t.add([RateRecord {
            date: d("2026-10-01"),
            valid_to: Some(d("2026-10-31")),
            base: c("GBP"),
            quote: c("UAH"),
            rate: dec!(55.5),
            source: RateSource::HmrcMonthly,
        }]);
        let s = [RateSource::HmrcMonthly];
        assert!(t.convert(dec!(1), &c("GBP"), &c("UAH"), d("2026-10-15"), &s).is_ok());
        assert!(t.convert(dec!(1), &c("GBP"), &c("UAH"), d("2026-11-01"), &s).is_err());
    }
}
