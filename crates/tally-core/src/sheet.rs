//! Evaluating a sheet: typed values per column, calculated columns, section
//! subtotals and sheet totals, with every exchange rate used listed.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Serialize;

use crate::formula::{self, Context, Expr, Value};
use crate::fx::{RateLeg, RateSource, RateTable};
use crate::money::{format_amount, round, Currency, RoundingScope};
use crate::ops::{ColumnKind, FixedRate, RateMode, RowKind};
use crate::project::{ColumnInfo, Project};
use crate::Result;

/// Rates older than this (relative to the sheet's date) are flagged.
pub const STALE_AFTER_DAYS: i64 = 7;
const MAX_WARNINGS: usize = 12;

#[derive(Clone, Debug, Serialize)]
pub struct SheetView {
    pub sheet_id: String,
    pub name: String,
    pub rates: RateMode,
    /// The date whose rates are used.
    pub as_of: NaiveDate,
    /// Rate sources chosen for this sheet (empty = the project's).
    pub sources: Vec<RateSource>,
    /// Rate sources actually used, in order of preference.
    pub effective_sources: Vec<RateSource>,
    pub fixed_rates: Vec<FixedRate>,
    /// The one rate per currency this sheet uses.
    pub rate_card: RateCard,
    pub columns: Vec<ColumnView>,
    pub rows: Vec<RowView>,
    /// Sheet totals, one per column (`None` where the column is not totalled).
    pub totals: Vec<Option<TotalView>>,
    /// Rates used, as published: `1 from = rate to`.
    pub rates_used: Vec<RateLeg>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ColumnView {
    /// Invariant ID: c1, c2, …
    pub col_id: String,
    pub position: f64,
    /// Display name, e.g. "Price (UAH)".
    pub name: String,
    /// Variable name for formulas and SQL, e.g. price_uah.
    pub var: String,
    /// For calculated columns: the formula written with variable names.
    pub formula_display: Option<String>,
    #[serde(flatten)]
    pub kind: ColumnKind,
    pub total: bool,
    pub style: crate::ops::Style,
    /// Formula problems that affect the whole column.
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RowView {
    pub row_id: String,
    pub position: f64,
    pub kind: RowKind,
    pub cells: Vec<CellView>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct CellView {
    /// What was typed (input columns and section headings).
    pub raw: Option<String>,
    /// Calculated value or subtotal, formatted to the currency's decimals.
    pub value: Option<String>,
    pub currency: Option<Currency>,
    pub error: Option<String>,
    /// Not an error, but probably a mistake (e.g. a price in two currencies).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TotalView {
    pub amount: String,
    pub currency: Option<Currency>,
    /// False when some rows could not be included.
    pub complete: bool,
}

/// Excel column letter for a 0-based index (used when exporting to XLSX).
pub fn col_letter(index: usize) -> String {
    let mut n = index + 1;
    let mut s = String::new();
    while n > 0 {
        let rem = (n - 1) % 26;
        s.insert(0, (b'A' + rem as u8) as char);
        n = (n - 1) / 26;
    }
    s
}

/// Turn a stored string into a value for its column.
fn input_value(kind: &ColumnKind, raw: Option<&String>) -> Value {
    let Some(raw) = raw else { return Value::Empty };
    match kind {
        ColumnKind::Text | ColumnKind::Link => Value::Text(raw.clone()),
        // Stored values are normalised, but a column whose type changed may hold
        // text such as "UAH 3,250.00" or "$ 71.50": read it leniently.
        ColumnKind::Number => match Decimal::from_str(raw) {
            Ok(d) => Value::Number(d),
            Err(_) => match crate::money::parse_amount(raw) {
                Ok((d, None)) => Value::Number(d),
                _ => Value::Error(format!("{raw:?} is not a number")),
            },
        },
        ColumnKind::Money { currency } => match Decimal::from_str(raw) {
            Ok(d) => Value::Money(d, currency.clone()),
            Err(_) => match crate::money::parse_amount(raw) {
                Ok((d, None)) => Value::Money(d, currency.clone()),
                Ok((d, Some(c))) if &c == currency => Value::Money(d, c),
                Ok((_, Some(c))) => Value::Error(format!("{raw:?} is in {c}, but this is a {currency} column")),
                Err(_) => Value::Error(format!("{raw:?} is not an amount")),
            },
        },
        ColumnKind::Date => crate::time::parse_date(raw).map_or_else(|e| Value::Error(e.to_string()), Value::Date),
        ColumnKind::DateTime => crate::time::parse_moment(raw).map_or_else(|e| Value::Error(e.to_string()), Value::Time),
        ColumnKind::Calc { .. } => Value::Empty,
    }
}

/// The sheet's rate card: every currency gets exactly one rate against the base
/// currency for the sheet's date, and all conversions go through it, so a currency
/// never has two rates in one sheet and cross rates always agree.
#[derive(Clone, Debug, Serialize)]
pub struct RateCard {
    pub base: Currency,
    /// False when the base was chosen automatically.
    pub base_chosen: bool,
    pub as_of: NaiveDate,
    pub entries: Vec<CardEntry>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CardEntry {
    pub currency: Currency,
    /// `1 base = rate currency`, as text.
    pub rate: Option<String>,
    /// Set by hand for this sheet (pegged).
    pub pinned: bool,
    /// Where the rate comes from (several when crossed through another currency).
    pub legs: Vec<RateLeg>,
    /// The date the rate applies to (the oldest of its legs).
    pub rate_date: Option<NaiveDate>,
    /// When Tally fetched it (the oldest of its legs), if known.
    pub fetched_at: Option<chrono::DateTime<chrono::Utc>>,
    pub error: Option<String>,
}

struct Converter<'a> {
    rates: &'a RateTable,
    base: Currency,
    fixed: Vec<(Currency, Currency, Decimal)>,
    as_of: NaiveDate,
    sources: Vec<RateSource>,
    card: RefCell<Vec<(CardEntry, std::result::Result<Decimal, String>)>>,
}

impl Converter<'_> {
    /// `1 base = rate c`, computed once per currency and remembered.
    fn rate_of(&self, c: &Currency) -> std::result::Result<Decimal, String> {
        if c == &self.base {
            return Ok(Decimal::ONE);
        }
        if let Some((_, r)) = self.card.borrow().iter().find(|(e, _)| &e.currency == c) {
            return r.clone();
        }
        let mut entry = CardEntry { currency: c.clone(), rate: None, pinned: false, legs: vec![], rate_date: None, fetched_at: None, error: None };
        let pinned = self.fixed.iter().find_map(|(b, q, r)| {
            if b == &self.base && q == c {
                Some(*r)
            } else if b == c && q == &self.base {
                Some(Decimal::ONE / r)
            } else {
                None
            }
        });
        let result = match pinned {
            Some(r) => {
                entry.pinned = true;
                Ok(r)
            }
            None => match self.rates.convert(Decimal::ONE, &self.base, c, self.as_of, &self.sources) {
                Ok(conv) => {
                    entry.rate_date = conv.legs.iter().map(|l| l.date).min();
                    entry.fetched_at = conv.legs.iter().filter_map(|l| self.rates.fetched_at(l)).min();
                    entry.legs = conv
                        .legs
                        .into_iter()
                        .map(|l| if l.inverted { RateLeg { from: l.to, to: l.from, inverted: false, ..l } } else { l })
                        .collect();
                    Ok(conv.amount)
                }
                Err(e) => Err(e.to_string()),
            },
        };
        match &result {
            Ok(r) => entry.rate = Some(r.normalize().round_dp(10).to_string()),
            Err(e) => entry.error = Some(e.clone()),
        }
        self.card.borrow_mut().push((entry, result.clone()));
        result
    }

    fn convert(&self, amount: Decimal, from: &Currency, to: &Currency) -> std::result::Result<Decimal, String> {
        if from == to {
            return Ok(amount);
        }
        let rf = self.rate_of(from)?;
        let rt = self.rate_of(to)?;
        // amount in base = amount / rf; in target = that × rt. Multiply first.
        Ok(amount * rt / rf)
    }
}

/// Currencies a sheet uses: money columns and the targets of `to()`.
fn sheet_currencies(columns: &[ColumnInfo], cols: &[formula::ColRef]) -> (Vec<Currency>, Vec<Currency>) {
    let mut all = Vec::new();
    let mut targets = Vec::new();
    fn walk(e: &Expr, targets: &mut Vec<Currency>) {
        match e {
            Expr::Call(f, args) => {
                if f == "to" {
                    if let Some(Expr::Cur(c)) = args.get(1) {
                        targets.push(c.clone());
                    }
                }
                args.iter().for_each(|a| walk(a, targets));
            }
            Expr::Neg(a) => walk(a, targets),
            Expr::Bin(_, a, b) => {
                walk(a, targets);
                walk(b, targets);
            }
            _ => {}
        }
    }
    for c in columns {
        match &c.spec.kind {
            ColumnKind::Money { currency } => all.push(currency.clone()),
            ColumnKind::Calc { formula: src, currency } => {
                if let Some(c) = currency {
                    targets.push(c.clone());
                }
                if let Ok(e) = formula::parse(src).and_then(|e| formula::resolve(&e, cols)) {
                    walk(&e, &mut targets);
                }
            }
            _ => {}
        }
    }
    all.extend(targets.iter().cloned());
    let mut unique = Vec::new();
    for c in all {
        if !unique.contains(&c) {
            unique.push(c);
        }
    }
    (unique, targets)
}

struct RowCtx<'a> {
    functions: &'a formula::Functions,
    columns: &'a [ColumnInfo],
    values: &'a HashMap<String, Value>,
    conv: &'a Converter<'a>,
    notes: std::cell::RefCell<Vec<(Vec<String>, String)>>,
}

impl Context for RowCtx<'_> {
    fn column(&self, id: &str) -> Option<Value> {
        if !self.columns.iter().any(|c| c.col_id == id) {
            return None;
        }
        Some(self.values.get(id).cloned().unwrap_or(Value::Empty))
    }

    fn convert(&self, amount: Decimal, from: &Currency, to: &Currency) -> std::result::Result<Decimal, String> {
        self.conv.convert(amount, from, to)
    }

    fn functions(&self) -> Option<&formula::Functions> {
        Some(self.functions)
    }

    fn var_of(&self, id: &str) -> Option<String> {
        self.columns.iter().find(|c| c.col_id == id).map(|c| c.spec.var.clone())
    }

    fn note(&self, cols: Vec<String>, msg: String) {
        self.notes.borrow_mut().push((cols, msg));
    }
}

/// Calculated columns in an order where each comes after the ones it uses.
fn calc_order(columns: &[ColumnInfo], cols: &[formula::ColRef]) -> (Vec<(usize, Expr)>, HashMap<usize, String>) {
    let mut parsed: Vec<(usize, Expr, Vec<usize>)> = Vec::new();
    let mut errors = HashMap::new();
    for (i, c) in columns.iter().enumerate() {
        if let ColumnKind::Calc { formula: src, .. } = &c.spec.kind {
            match formula::parse(src).and_then(|e| formula::resolve(&e, cols)) {
                Ok(e) => {
                    let deps = formula::refs(&e).iter().filter_map(|id| columns.iter().position(|c| &c.col_id == id)).collect();
                    parsed.push((i, e, deps));
                }
                Err(e) => {
                    errors.insert(i, format!("formula: {e}"));
                }
            }
        }
    }
    let calc: HashSet<usize> = parsed.iter().map(|(i, ..)| *i).collect();
    let mut done: HashSet<usize> = HashSet::new();
    let mut order = Vec::new();
    loop {
        let before = order.len();
        for (i, e, deps) in &parsed {
            if done.contains(i) {
                continue;
            }
            if deps.iter().all(|d| !calc.contains(d) || done.contains(d)) {
                done.insert(*i);
                order.push((*i, e.clone()));
            }
        }
        if order.len() == before {
            break;
        }
    }
    for (i, ..) in &parsed {
        if !done.contains(i) {
            errors.insert(*i, "formulas refer to each other in a loop".into());
        }
    }
    (order, errors)
}

#[derive(Default)]
struct Sum {
    number: Decimal,
    money: Option<(Decimal, Currency)>,
    any: bool,
    complete: bool,
    mixed: bool,
}

impl Sum {
    fn new() -> Self {
        Sum { complete: true, ..Default::default() }
    }

    fn add(&mut self, v: &Value) {
        match v {
            Value::Empty | Value::Text(_) | Value::Currency(_) | Value::Date(_) | Value::Time(_) => {}
            Value::Error(_) => self.complete = false,
            Value::Number(n) => {
                self.number += n;
                self.any = true;
            }
            Value::Money(n, c) => {
                self.any = true;
                match &mut self.money {
                    None => self.money = Some((*n, c.clone())),
                    Some((t, tc)) if tc == c => *t += n,
                    Some(_) => self.mixed = true,
                }
            }
        }
    }

    fn view(&self) -> Option<TotalView> {
        if !self.any {
            return None;
        }
        if self.mixed {
            return Some(TotalView { amount: "mixed currencies".into(), currency: None, complete: false });
        }
        Some(match &self.money {
            Some((t, c)) => TotalView { amount: format_amount(round(*t, c, Default::default()), c), currency: Some(c.clone()), complete: self.complete },
            None => TotalView { amount: self.number.normalize().to_string(), currency: None, complete: self.complete },
        })
    }
}

impl Project {
    pub fn sheet_view(&self, sheet_id: &str, rates: &RateTable, today: NaiveDate) -> Result<SheetView> {
        self.sheet_view_with(sheet_id, rates, today, None)
    }

    /// Try a formula before saving it: its result on the first rows that give one.
    /// `col_id` is the column being edited (or `None` for a new column).
    pub fn preview_formula(
        &self,
        sheet_id: &str,
        col_id: Option<&str>,
        source: &str,
        currency: Option<Currency>,
        rates: &RateTable,
        today: NaiveDate,
    ) -> Result<Vec<String>> {
        let id = col_id.unwrap_or("__preview").to_string();
        let expr = formula::parse(source).map_err(crate::Error::InvalidValue)?;
        let mut cols = self.col_refs(sheet_id)?;
        if col_id.is_none() {
            cols.push(formula::ColRef { id: id.clone(), var: "__preview".into(), display: "__preview".into() });
        }
        let resolved = formula::resolve(&expr, &cols).map_err(crate::Error::InvalidValue)?;
        if formula::refs(&resolved).contains(&id) {
            return Err(crate::Error::InvalidValue("the formula refers to its own column".into()));
        }
        let extra = ColumnInfo {
            col_id: id.clone(),
            position: f64::MAX,
            spec: crate::ops::ColumnSpec {
                name: "__preview".into(),
                var: "__preview".into(),
                kind: ColumnKind::Calc { formula: resolved.to_string(), currency },
                total: false,
                style: Default::default(),
            },
        };
        let view = self.sheet_view_with(sheet_id, rates, today, Some(extra))?;
        let j = view.columns.iter().position(|c| c.col_id == id).expect("preview column");
        let mut out = Vec::new();
        for (n, r) in view.rows.iter().enumerate() {
            if r.kind != RowKind::Item {
                continue;
            }
            let c = &r.cells[j];
            let text = match (&c.value, &c.currency, &c.error) {
                (_, _, Some(e)) => format!("row {}: {e}", n + 1),
                (Some(v), Some(cur), _) => format!("row {}: {v} {cur}", n + 1),
                (Some(v), None, _) => format!("row {}: {v}", n + 1),
                _ => continue,
            };
            out.push(text);
            if out.len() == 3 {
                break;
            }
        }
        Ok(out)
    }

    fn sheet_view_with(&self, sheet_id: &str, rates: &RateTable, today: NaiveDate, extra: Option<ColumnInfo>) -> Result<SheetView> {
        let sheet = self.sheet(sheet_id)?;
        let settings = self.settings();
        let mut columns = self.columns(sheet_id)?;
        if let Some(x) = extra {
            match columns.iter_mut().find(|c| c.col_id == x.col_id) {
                Some(c) => c.spec.kind = x.spec.kind,
                None => columns.push(x),
            }
        }
        let rows = self.rows(sheet_id)?;
        let stored = self.cells(sheet_id)?;
        let as_of = sheet.rates.as_of(today);
        let effective_sources = if sheet.sources.is_empty() {
            settings.rate_sources.clone()
        } else {
            // The sheet's choice first, then the project's others as fallbacks
            // (e.g. ECB first, NBU for hryvnia, which ECB does not publish).
            let mut s = sheet.sources.clone();
            for x in &settings.rate_sources {
                if !s.contains(x) {
                    s.push(*x);
                }
            }
            s
        };
        let col_refs: Vec<formula::ColRef> = columns
            .iter()
            .map(|c| formula::ColRef { id: c.col_id.clone(), var: c.spec.var.clone(), display: c.spec.name.clone() })
            .collect();
        let (order, column_errors) = calc_order(&columns, &col_refs);

        // The rate card's base: chosen for the sheet, else the currency most
        // formulas convert to, else the project's reporting currency.
        let (currencies, targets) = sheet_currencies(&columns, &col_refs);
        let base = sheet.base.clone().unwrap_or_else(|| {
            let mut best: Option<(&Currency, usize)> = None;
            for t in &targets {
                let n = targets.iter().filter(|x| *x == t).count();
                if best.map_or(true, |(_, m)| n > m) {
                    best = Some((t, n));
                }
            }
            best.map(|(c, _)| c.clone()).unwrap_or_else(|| settings.reporting_currency.clone())
        });
        let fixed = sheet
            .fixed_rates
            .iter()
            .filter_map(|f| Decimal::from_str(&f.rate).ok().map(|r| (f.base.clone(), f.quote.clone(), r)))
            .collect();
        let conv = Converter { rates, base: base.clone(), fixed, as_of, sources: effective_sources.clone(), card: RefCell::new(vec![]) };
        // Every currency the sheet uses gets its line on the card, used yet or not.
        for c in &currencies {
            let _ = conv.rate_of(c);
        }
        let per_line = settings.rounding_scope == RoundingScope::PerLine;
        let mut warnings: Vec<String> = Vec::new();
        let mut warn = |w: String| {
            if !warnings.contains(&w) && warnings.len() < MAX_WARNINGS {
                warnings.push(w);
            }
        };
        for (i, e) in &column_errors {
            warn(format!("{}: {e}", columns[*i].spec.name));
        }

        // Evaluate every row.
        let mut row_values: Vec<Vec<Value>> = Vec::with_capacity(rows.len());
        // Warnings on single cells: (row index, column ID) -> message.
        let mut cell_notes: HashMap<(usize, String), String> = HashMap::new();
        // The same note on many rows is reported once: (column name, message) -> rows.
        let mut grouped: Vec<((String, String), Vec<usize>)> = Vec::new();
        for (n, row) in rows.iter().enumerate() {
            let mut values: HashMap<String, Value> = HashMap::new();
            if row.kind == RowKind::Item {
                for c in &columns {
                    values.insert(c.col_id.clone(), input_value(&c.spec.kind, stored.get(&(row.row_id.clone(), c.col_id.clone()))));
                }
                for (i, expr) in &order {
                    let v = {
                        let ctx = RowCtx { functions: self.functions(), columns: &columns, values: &values, conv: &conv, notes: Default::default() };
                        let v = formula::eval(expr, &ctx);
                        for (cols, msg) in ctx.notes.into_inner() {
                            let key = (columns[*i].spec.name.clone(), msg.clone());
                            match grouped.iter_mut().find(|(k, _)| *k == key) {
                                Some((_, rows)) => rows.push(n + 1),
                                None => grouped.push((key, vec![n + 1])),
                            }
                            for id in cols {
                                cell_notes.entry((n, id)).or_insert_with(|| msg.clone());
                            }
                            cell_notes.entry((n, columns[*i].col_id.clone())).or_insert(msg);
                        }
                        v
                    };
                    // A calculated column with a currency gives amounts in it: other
                        // currencies are converted at the rate card, plain numbers taken as it.
                    let v = match (&columns[*i].spec.kind, v) {
                        (ColumnKind::Calc { currency: Some(want), .. }, Value::Money(x, c)) if &c != want => {
                            match conv.convert(x, &c, want) {
                                Ok(y) => Value::Money(y, want.clone()),
                                Err(e) => Value::Error(e),
                            }
                        }
                        (ColumnKind::Calc { currency: Some(want), .. }, Value::Number(x)) => Value::Money(x, want.clone()),
                        (_, other) => other,
                    };
                    let v = match v {
                        Value::Money(x, c) if per_line => Value::Money(round(x, &c, settings.rounding), c),
                        Value::Text(_) | Value::Currency(_) => Value::Error("the formula gives text, not a value".into()),
                        other => other,
                    };
                    if let Value::Error(e) = &v {
                        warn(format!("{}, row {}: {e}", columns[*i].spec.name, n + 1));
                    }
                    values.insert(columns[*i].col_id.clone(), v);
                }
                for (i, e) in &column_errors {
                    values.insert(columns[*i].col_id.clone(), Value::Error(e.clone()));
                }
            }
            row_values.push(columns.iter().map(|c| values.remove(&c.col_id).unwrap_or(Value::Empty)).collect());
        }

        for ((col, msg), rows) in grouped {
            let at = match rows.len() {
                1 => format!("row {}", rows[0]),
                n if n <= 6 => format!("rows {}", rows.iter().map(|r| r.to_string()).collect::<Vec<_>>().join(", ")),
                n => format!("{n} rows ({}, …)", rows[..4].iter().map(|r| r.to_string()).collect::<Vec<_>>().join(", ")),
            };
            warn(format!("{col}, {at}: {msg}"));
        }

        // Section subtotals and sheet totals.
        let mut totals: Vec<Sum> = columns.iter().map(|_| Sum::new()).collect();
        let mut section_sums: HashMap<usize, Vec<Sum>> = HashMap::new();
        let mut current: Option<usize> = None;
        for (n, row) in rows.iter().enumerate() {
            if row.kind == RowKind::Section {
                current = Some(n);
                section_sums.insert(n, columns.iter().map(|_| Sum::new()).collect());
                continue;
            }
            for (j, c) in columns.iter().enumerate() {
                if !c.spec.total {
                    continue;
                }
                totals[j].add(&row_values[n][j]);
                if let Some(s) = current.and_then(|s| section_sums.get_mut(&s)) {
                    s[j].add(&row_values[n][j]);
                }
            }
        }

        let row_views = rows
            .iter()
            .enumerate()
            .map(|(n, row)| {
                let cells = columns
                    .iter()
                    .enumerate()
                    .map(|(j, c)| {
                        let raw = stored.get(&(row.row_id.clone(), c.col_id.clone())).cloned();
                        if row.kind == RowKind::Section {
                            let sub = section_sums.get(&n).and_then(|s| if c.spec.total { s[j].view() } else { None });
                            return CellView {
                                raw: if c.spec.kind.is_input() { raw } else { None },
                                value: sub.as_ref().map(|t| t.amount.clone()),
                                currency: sub.and_then(|t| t.currency),
                                error: None,
                                warning: None,
                            };
                        }
                        let (value, currency, error) = match &row_values[n][j] {
                            Value::Money(x, cur) if !c.spec.kind.is_input() => {
                                (Some(format_amount(round(*x, cur, settings.rounding), cur)), Some(cur.clone()), None)
                            }
                            Value::Number(x) if !c.spec.kind.is_input() => (Some(x.normalize().to_string()), None, None),
                            Value::Date(d) if !c.spec.kind.is_input() => (Some(crate::time::store_date(d)), None, None),
                            Value::Time(t) if !c.spec.kind.is_input() => (Some(crate::time::store_moment(t)), None, None),
                            Value::Money(_, cur) => (None, Some(cur.clone()), None),
                            Value::Error(e) => (None, None, Some(e.clone())),
                            _ => (None, None, None),
                        };
                        let warning = cell_notes.get(&(n, c.col_id.clone())).cloned();
                        CellView { raw: if c.spec.kind.is_input() { raw } else { None }, value, currency, error, warning }
                    })
                    .collect();
                RowView { row_id: row.row_id.clone(), position: row.position, kind: row.kind, cells }
            })
            .collect();

        let mut card_entries: Vec<CardEntry> = conv.card.into_inner().into_iter().map(|(e, _)| e).collect();
        card_entries.sort_by(|a, b| a.currency.cmp(&b.currency));
        for e in &card_entries {
            if let Some(err) = &e.error {
                warn(format!("rate card: {err}"));
            }
        }
        let mut legs: Vec<RateLeg> = Vec::new();
        for e in &card_entries {
            if e.pinned {
                legs.push(RateLeg {
                    from: base.clone(),
                    to: e.currency.clone(),
                    rate: Decimal::from_str(e.rate.as_deref().unwrap_or("0")).unwrap_or_default(),
                    inverted: false,
                    source: RateSource::Manual,
                    date: as_of,
                });
            }
            for l in &e.legs {
                if !legs.contains(l) {
                    legs.push(l.clone());
                }
            }
        }
        legs.sort_by(|a, b| a.source.cmp(&b.source).then(a.from.cmp(&b.from)).then(a.to.cmp(&b.to)));
        let stale: Vec<String> = legs
            .iter()
            .filter(|l| !matches!(l.source, RateSource::HmrcMonthly | RateSource::Manual))
            .filter(|l| (as_of - l.date).num_days() > STALE_AFTER_DAYS)
            .map(|l| format!("{} {}→{} from {}", l.source, l.from, l.to, l.date))
            .collect();
        if !stale.is_empty() {
            warn(format!("rates older than {STALE_AFTER_DAYS} days: {}", stale.join(", ")));
        }

        Ok(SheetView {
            sheet_id: sheet.sheet_id,
            name: sheet.name,
            rates: sheet.rates,
            as_of,
            sources: sheet.sources,
            effective_sources,
            fixed_rates: sheet.fixed_rates,
            rate_card: RateCard { base, base_chosen: sheet.base.is_some(), as_of, entries: card_entries },
            columns: columns
                .iter()
                .enumerate()
                .map(|(i, c)| ColumnView {
                    col_id: c.col_id.clone(),
                    position: c.position,
                    name: c.spec.name.clone(),
                    var: c.spec.var.clone(),
                    formula_display: match &c.spec.kind {
                        ColumnKind::Calc { formula: src, .. } => Some(
                            formula::parse(src)
                                .and_then(|e| formula::resolve(&e, &col_refs))
                                .map(|e| formula::render(&e, &col_refs))
                                .unwrap_or_else(|_| src.clone()),
                        ),
                        _ => None,
                    },
                    kind: c.spec.kind.clone(),
                    total: c.spec.total,
                    style: c.spec.style.clone(),
                    error: column_errors.get(&i).cloned(),
                })
                .collect(),
            rows: row_views,
            totals: columns.iter().enumerate().map(|(j, c)| if c.spec.total { totals[j].view() } else { None }).collect(),
            rates_used: legs,
            warnings,
        })
    }
}

impl Project {
    /// Write every sheet into DuckDB as a plain table with one column per sheet
    /// column (calculated values included), named after the sheet, e.g. `drone`.
    /// Item rows only, plus `row` and `section`. Returns the table names.
    pub fn materialize(&self, rates: &RateTable, today: NaiveDate) -> Result<Vec<String>> {
        let conn = self.connection();
        let mut names = Vec::new();
        for sheet in self.sheets(false)? {
            let view = self.sheet_view(&sheet.sheet_id, rates, today)?;
            let mut table = ident(&sheet.name);
            let mut n = 2;
            while names.contains(&table) {
                table = format!("{}_{n}", ident(&sheet.name));
                n += 1;
            }
            let mut cols = vec!["\"row\" INTEGER".to_string(), "\"section\" VARCHAR".to_string()];
            let mut used = vec!["row".to_string(), "section".to_string()];
            let mut col_names = Vec::new();
            for (j, c) in view.columns.iter().enumerate() {
                let mut name = c.var.clone();
                while used.contains(&name) {
                    name.push('_');
                }
                used.push(name.clone());
                // SQL type, and how a value is cast into it (calculated columns may give anything).
                let ty = match c.kind {
                    ColumnKind::Text | ColumnKind::Link => "VARCHAR",
                    ColumnKind::Date => "DATE",
                    ColumnKind::DateTime => "TIMESTAMP", // UTC (TIMESTAMPTZ needs ICU)
                    // Whatever the formula gives: judged by the first value.
                    ColumnKind::Calc { .. } => match view.rows.iter().find_map(|r| r.cells[j].value.as_deref()) {
                        Some(v) if Decimal::from_str(v).is_err() && crate::time::parse_date(v).is_ok() && v.len() == 10 => "DATE",
                        Some(v) if Decimal::from_str(v).is_err() && crate::time::parse_moment(v).is_ok() => "TIMESTAMP",
                        _ => "DECIMAL(38, 10)",
                    },
                    _ => "DECIMAL(38, 10)",
                };
                let cast = if ty == "VARCHAR" { "?".to_string() } else { format!("TRY_CAST(? AS {ty})") };
                cols.push(format!("\"{name}\" {ty}"));
                col_names.push((name, cast));
            }
            conn.execute_batch(&format!("DROP TABLE IF EXISTS \"{table}\"; CREATE TABLE \"{table}\" ({});", cols.join(", ")))?;
            let placeholders: Vec<String> = std::iter::once("?".to_string())
                .chain(std::iter::once("?".to_string()))
                .chain(col_names.iter().map(|(_, cast)| cast.to_string()))
                .collect();
            let sql = format!("INSERT INTO \"{table}\" VALUES ({})", placeholders.join(", "));
            let mut section: Option<String> = None;
            for (i, r) in view.rows.iter().enumerate() {
                if r.kind == RowKind::Section {
                    section = r.cells.iter().find_map(|c| c.raw.clone());
                    continue;
                }
                let mut values: Vec<Option<String>> = vec![Some((i + 1).to_string()), section.clone()];
                for j in 0..col_names.len() {
                    let cell = &r.cells[j];
                    values.push(cell.raw.clone().or_else(|| cell.value.clone()));
                }
                conn.execute(&sql, duckdb::params_from_iter(values.iter()))?;
            }
            names.push(table);
        }
        Ok(names)
    }
}

/// A plain SQL name from a label: "Total USD" -> total_usd.
fn ident(s: &str) -> String {
    let mut out: String = s.trim().to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect();
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    let out = out.trim_matches('_').to_string();
    if out.is_empty() || out.starts_with(|c: char| c.is_ascii_digit()) {
        format!("t_{out}")
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::RateRecord;
    use crate::ops::{FixedRate, Op};
    use crate::project::tests::{add_row, cur, spending_project};
    use rust_decimal_macros::dec;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    /// ECB has no hryvnia; NBU does.
    fn rates() -> RateTable {
        let mut t = RateTable::new();
        let r = |base: &str, quote: &str, rate, source| RateRecord {
            date: d("2026-10-22"),
            valid_to: None,
            base: cur(base),
            quote: cur(quote),
            rate,
            source,
        };
        t.add([
            r("EUR", "USD", dec!(1.10), RateSource::Ecb),
            r("USD", "UAH", dec!(44.00), RateSource::Nbu),
            r("EUR", "UAH", dec!(48.40), RateSource::Nbu),
        ]);
        t
    }

    fn cell<'a>(v: &'a SheetView, row: usize, col: &str) -> &'a CellView {
        let j = v.columns.iter().position(|c| c.col_id == col).unwrap();
        &v.rows[row].cells[j]
    }

    #[test]
    fn spending_list_with_sections() {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = spending_project(tmp.path());
        add_row(&mut p, "h1", &[("item", "Frame")]);
        p.apply(Op::SetRowKind { sheet_id: "s".into(), row_id: "h1".into(), kind: RowKind::Section }).unwrap();
        add_row(&mut p, "a", &[("item", "Carbon plates"), ("uah", "3250"), ("qty", "2")]);
        add_row(&mut p, "b", &[("item", "Aluminium"), ("usd", "10"), ("qty", "1")]);
        add_row(&mut p, "h2", &[("item", "Electronics")]);
        p.apply(Op::SetRowKind { sheet_id: "s".into(), row_id: "h2".into(), kind: RowKind::Section }).unwrap();
        add_row(&mut p, "c", &[("item", "Starlink mini"), ("usd", "250"), ("qty", "1")]);
        add_row(&mut p, "e", &[("item", "Ethernet switch"), ("qty", "1")]);

        let v = p.sheet_view("s", &rates(), d("2026-10-22")).unwrap();
        assert!(v.warnings.is_empty(), "{:?}", v.warnings);
        assert_eq!(v.columns.iter().map(|c| c.var.as_str()).collect::<Vec<_>>(), ["item", "uah", "usd", "unit_usd", "qty", "total_usd"]);
        assert_eq!(v.columns[5].formula_display.as_deref(), Some("unit_usd * qty"));
        // 3250 UAH / 44 = 73.86 USD each, × 2 = 147.72.
        assert_eq!(cell(&v, 1, "unit").value.as_deref(), Some("73.86"));
        assert_eq!(cell(&v, 1, "total").value.as_deref(), Some("147.72"));
        assert_eq!(cell(&v, 2, "unit").value.as_deref(), Some("10.00"));
        // Section subtotals sit on the heading rows.
        assert_eq!(cell(&v, 0, "total").value.as_deref(), Some("157.72"));
        assert_eq!(cell(&v, 3, "total").value.as_deref(), Some("250.00"));
        // No price: no total, and no error.
        assert_eq!(cell(&v, 5, "total").value, None);
        assert_eq!(cell(&v, 5, "total").error, None);
        let total = v.totals[5].as_ref().unwrap();
        assert_eq!((total.amount.as_str(), total.complete), ("407.72", true));
        // Only the NBU rate was needed.
        assert_eq!(v.rates_used.len(), 1);
        assert_eq!(v.rates_used[0].source, RateSource::Nbu);
    }

    #[test]
    fn ecb_first_with_nbu_for_hryvnia() {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = spending_project(tmp.path());
        p.apply(Op::SetSheetSources { sheet_id: "s".into(), sources: vec![RateSource::Ecb] }).unwrap();
        p.apply(Op::AddColumn {
            sheet_id: "s".into(),
            col_id: "eur".into(),
            position: 10.0,
            spec: crate::ops::ColumnSpec {
                name: "EUR".into(),
                kind: ColumnKind::Calc { formula: "to(total_usd, EUR)".into(), currency: None },
                style: Default::default(),
                total: false,
                var: String::new(),
            },
        })
        .unwrap();
        add_row(&mut p, "a", &[("uah", "4400"), ("qty", "1")]);
        let v = p.sheet_view("s", &rates(), d("2026-10-22")).unwrap();
        assert_eq!(v.effective_sources[0], RateSource::Ecb);
        assert_eq!(cell(&v, 0, "total").value.as_deref(), Some("100.00"));
        // USD -> EUR uses ECB, UAH -> USD falls back to NBU.
        assert_eq!(cell(&v, 0, "eur").value.as_deref(), Some("90.91"));
        let sources: HashSet<_> = v.rates_used.iter().map(|l| l.source).collect();
        assert_eq!(sources, HashSet::from([RateSource::Ecb, RateSource::Nbu]));
    }

    #[test]
    fn rate_card_gives_one_rate_per_currency() {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = spending_project(tmp.path());
        p.apply(Op::SetSheetSources { sheet_id: "s".into(), sources: vec![RateSource::Ecb] }).unwrap();
        p.apply(Op::AddColumn {
            sheet_id: "s".into(),
            col_id: "eur".into(),
            position: 10.0,
            spec: crate::ops::ColumnSpec { name: "EUR".into(), var: String::new(), kind: ColumnKind::Calc { formula: "to(uah, EUR)".into(), currency: None }, total: false , style: Default::default()},
        })
        .unwrap();
        add_row(&mut p, "a", &[("uah", "4400"), ("qty", "1")]);
        let v = p.sheet_view("s", &rates(), d("2026-10-22")).unwrap();
        let card = &v.rate_card;
        // Base chosen automatically: the currency formulas convert to most (USD).
        assert_eq!(card.base.code(), "USD");
        assert!(!card.base_chosen);
        let codes: Vec<_> = card.entries.iter().map(|e| e.currency.code()).collect();
        assert_eq!(codes, ["EUR", "UAH"]);
        let uah = &card.entries[1];
        assert_eq!(uah.rate.as_deref(), Some("44"));
        assert_eq!(uah.legs[0].source, RateSource::Nbu);
        // UAH -> EUR goes through the card (UAH -> USD at NBU, USD -> EUR at ECB),
        // never NBU's direct EUR rate, so it agrees with the USD column.
        assert_eq!(cell(&v, 0, "eur").value.as_deref(), Some("90.91"));
    }

    #[test]
    fn fixed_rate_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = spending_project(tmp.path());
        p.apply(Op::SetSheetFixedRates {
            sheet_id: "s".into(),
            rates: vec![FixedRate { base: cur("USD"), quote: cur("UAH"), rate: "44.92".into() }],
        })
        .unwrap();
        add_row(&mut p, "a", &[("uah", "4492"), ("qty", "1")]);
        let v = p.sheet_view("s", &rates(), d("2026-10-22")).unwrap();
        assert_eq!(cell(&v, 0, "unit").value.as_deref(), Some("100.00"));
        assert_eq!(v.rates_used[0].source, RateSource::Manual);
    }

    #[test]
    fn errors_show_in_cells_and_totals() {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = spending_project(tmp.path());
        add_row(&mut p, "a", &[("uah", "100"), ("qty", "1")]);
        // No rates at all.
        let v = p.sheet_view("s", &RateTable::new(), d("2026-10-22")).unwrap();
        assert!(cell(&v, 0, "unit").error.as_deref().unwrap().contains("UAH"));
        assert!(!v.warnings.is_empty());
        assert!(v.totals[5].is_none() || !v.totals[5].as_ref().unwrap().complete);
    }

    #[test]
    fn sheets_become_sql_tables() {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = spending_project(tmp.path());
        add_row(&mut p, "h", &[("item", "Frame")]);
        p.apply(Op::SetRowKind { sheet_id: "s".into(), row_id: "h".into(), kind: RowKind::Section }).unwrap();
        add_row(&mut p, "a", &[("item", "Plates"), ("uah", "4400"), ("qty", "2")]);
        let tables = p.materialize(&rates(), d("2026-10-22")).unwrap();
        assert_eq!(tables, ["drone"]);
        let (section, total): (String, String) = p
            .connection()
            .query_row("SELECT section, CAST(total_usd AS VARCHAR) FROM drone", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(section, "Frame");
        assert_eq!(Decimal::from_str(&total).unwrap(), dec!(200));
    }

    #[test]
    fn formula_preview() {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = spending_project(tmp.path());
        add_row(&mut p, "a", &[("uah", "4400"), ("qty", "3")]);
        let got = p.preview_formula("s", None, "total_usd * 2", None, &rates(), d("2026-10-22")).unwrap();
        assert_eq!(got, ["row 1: 600.00 USD"]);
        assert!(p.preview_formula("s", None, "nope * 2", None, &rates(), d("2026-10-22")).is_err());
        assert!(p.preview_formula("s", Some("total"), "total_usd * 2", None, &rates(), d("2026-10-22")).is_err(), "itself");
        // The spending template works out of the box.
        let id = "t".to_string();
        p.apply(Op::Batch { ops: crate::templates::sheet_ops("spending", &id, "Kit") }).unwrap();
        let cols = p.columns("t").unwrap();
        assert_eq!(cols.iter().map(|c| c.col_id.as_str()).collect::<Vec<_>>(), ["c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8"]);
        assert_eq!(cols[5].spec.kind, ColumnKind::Calc { formula: "coalesce(c4, c5)".into(), currency: Some(cur("USD")) });
    }

    #[test]
    fn retyped_columns_read_amounts_with_currencies() {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = spending_project(tmp.path());
        // A text column holding pasted amounts, then turned into money.
        p.apply(Op::AddColumn {
            sheet_id: "s".into(),
            col_id: "pasted".into(),
            position: 0.5,
            spec: crate::ops::ColumnSpec { name: "Pasted".into(), var: String::new(), kind: ColumnKind::Text, total: false , style: Default::default()},
        })
        .unwrap();
        add_row(&mut p, "a", &[("pasted", "UAH 3,250.00")]);
        add_row(&mut p, "b", &[("pasted", "$ 71.50")]);
        p.apply(Op::UpdateColumn {
            sheet_id: "s".into(),
            col_id: "pasted".into(),
            spec: crate::ops::ColumnSpec {
                name: "Pasted".into(),
                var: "pasted".into(),
                kind: ColumnKind::Money { currency: cur("UAH") },
                total: false,
                style: Default::default(),
            },
        })
        .unwrap();
        // Converted on the type change where it fits; kept as typed where it doesn't.
        let cells = p.cells("s").unwrap();
        assert_eq!(cells[&("a".into(), "pasted".into())], "3250");
        assert_eq!(cells[&("b".into(), "pasted".into())], "$ 71.50");
        let v = p.sheet_view("s", &rates(), d("2026-10-22")).unwrap();
        assert!(cell(&v, 1, "pasted").error.as_deref().unwrap().contains("USD"));
        // Undo puts the text back.
        p.undo().unwrap();
        assert_eq!(p.cells("s").unwrap()[&("a".into(), "pasted".into())], "UAH 3,250.00");
        assert!(matches!(input_value(&ColumnKind::Money { currency: cur("UAH") }, Some(&"UAH 3,250.00".to_string())), Value::Money(_, _)));
    }

    #[test]
    fn money_typed_calc_columns_convert() {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = spending_project(tmp.path());
        // Unit price in USD with no to(): the column's currency converts the UAH price.
        p.apply(Op::UpdateColumn {
            sheet_id: "s".into(),
            col_id: "unit".into(),
            spec: crate::ops::ColumnSpec {
                name: "Unit USD".into(),
                var: "unit_usd".into(),
                kind: ColumnKind::Calc { formula: "coalesce(uah, usd)".into(), currency: Some(cur("USD")) },
                total: false,
                style: Default::default(),
            },
        })
        .unwrap();
        add_row(&mut p, "a", &[("uah", "4400"), ("qty", "2")]);
        add_row(&mut p, "b", &[("usd", "250"), ("qty", "1")]);
        let v = p.sheet_view("s", &rates(), d("2026-10-22")).unwrap();
        assert_eq!(cell(&v, 0, "unit").value.as_deref(), Some("100.00"), "{:?} {:?}", cell(&v, 0, "unit"), v.warnings);
        assert_eq!(cell(&v, 0, "unit").currency.as_ref().unwrap().code(), "USD");
        assert_eq!(cell(&v, 1, "unit").value.as_deref(), Some("250.00"));
        assert_eq!(v.totals[5].as_ref().unwrap().amount, "450.00");
    }

    #[test]
    fn coalesce_with_two_values_is_flagged() {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = spending_project(tmp.path());
        p.apply(Op::UpdateColumn {
            sheet_id: "s".into(),
            col_id: "unit".into(),
            spec: crate::ops::ColumnSpec {
                name: "Unit USD".into(),
                var: "unit_usd".into(),
                kind: ColumnKind::Calc { formula: "coalesce(uah, usd)".into(), currency: Some(cur("USD")) },
                total: false,
                style: Default::default(),
            },
        })
        .unwrap();
        add_row(&mut p, "a", &[("uah", "4400"), ("usd", "90"), ("qty", "2")]);
        add_row(&mut p, "b", &[("usd", "250"), ("qty", "1")]);
        let v = p.sheet_view("s", &rates(), d("2026-10-22")).unwrap();
        let w = cell(&v, 0, "unit").warning.clone().unwrap();
        assert!(w.contains("both filled in") && w.contains("using"), "{w}");
        assert!(cell(&v, 0, "uah").warning.is_some() && cell(&v, 0, "usd").warning.is_some());
        assert_eq!(cell(&v, 0, "unit").value.as_deref(), Some("100.00"));
        assert!(cell(&v, 1, "unit").warning.is_none());
        assert!(v.warnings.iter().any(|x| x.starts_with("Unit USD, row 1:")), "{:?}", v.warnings);
    }

    #[test]
    fn letters() {
        assert_eq!(col_letter(0), "A");
        assert_eq!(col_letter(25), "Z");
        assert_eq!(col_letter(26), "AA");
        assert_eq!(col_letter(27), "AB");
    }
}
