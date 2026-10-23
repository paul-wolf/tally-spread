//! Quick analysis of a sheet's columns: values over time (bucketed when there
//! are more points than a chart can show), distributions (quartiles, histogram,
//! normal fit) and group summaries. Nothing here changes the sheet.

use std::collections::HashMap;
use std::str::FromStr;

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::Serialize;

use crate::ops::{ColumnKind, RowKind};
use crate::sheet::{CellView, ColumnView, SheetView};
use crate::{Error, Result};

// ------------------------------------------------------------------ reading values

fn column<'a>(view: &'a SheetView, col_id: &str) -> Result<(usize, &'a ColumnView)> {
    view.columns
        .iter()
        .enumerate()
        .find(|(_, c)| c.col_id == col_id)
        .ok_or_else(|| Error::NotFound(format!("column {col_id}")))
}

fn text(cell: &CellView) -> Option<&str> {
    cell.raw.as_deref().or(cell.value.as_deref()).filter(|s| !s.trim().is_empty())
}

/// Seconds since 1970 for a stored date or moment.
fn seconds(s: &str) -> Option<f64> {
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(t.timestamp_micros() as f64 / 1e6);
    }
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .map(|d| d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp() as f64)
}

pub fn is_time(c: &ColumnView, view: &SheetView, j: usize) -> bool {
    match c.kind {
        ColumnKind::Date | ColumnKind::DateTime => true,
        // A calculated column of dates or times, judged by its first value.
        ColumnKind::Calc { .. } => view
            .rows
            .iter()
            .find_map(|r| r.cells[j].value.as_deref())
            .is_some_and(|v| Decimal::from_str(v).is_err() && seconds(v).is_some()),
        _ => false,
    }
}

/// A cell as a number: amounts and numbers as they are, dates and times as seconds.
fn number(c: &ColumnView, cell: &CellView, time: bool) -> Option<f64> {
    let s = text(cell)?;
    if time {
        return seconds(s);
    }
    match c.kind {
        ColumnKind::Number | ColumnKind::Money { .. } | ColumnKind::Calc { .. } => Decimal::from_str(s).ok()?.to_f64(),
        _ => None,
    }
}

fn items(view: &SheetView) -> impl Iterator<Item = (usize, &crate::sheet::RowView)> {
    view.rows.iter().enumerate().filter(|(_, r)| r.kind == RowKind::Item)
}

/// The column values are plotted against: the first date & time column, if any.
pub fn default_x(view: &SheetView) -> Option<String> {
    let pick = |want: ColumnKind| view.columns.iter().find(|c| c.kind == want).map(|c| c.col_id.clone());
    pick(ColumnKind::DateTime).or_else(|| pick(ColumnKind::Date))
}

// ------------------------------------------------------------------ over time

#[derive(Debug, Serialize)]
pub struct Line {
    pub col_id: String,
    pub name: String,
    pub currency: Option<String>,
    /// One value per x; `None` where there is nothing.
    pub avg: Vec<Option<f64>>,
    /// Lowest and highest value in each bucket (only when bucketed).
    pub min: Option<Vec<Option<f64>>>,
    pub max: Option<Vec<Option<f64>>>,
}

#[derive(Debug, Serialize)]
pub struct Series {
    /// Seconds since 1970 when `x_is_time`, otherwise row numbers.
    pub x: Vec<f64>,
    pub x_is_time: bool,
    pub x_name: String,
    /// Bucket width in seconds (or rows), when points were grouped.
    pub bucket: Option<f64>,
    pub points: usize,
    pub lines: Vec<Line>,
}

/// Values of `cols` against `x` (a time column; `None` = the first one, or row
/// order when there is none). More than `max_points` points are grouped into
/// equal buckets, each with its average, lowest and highest value.
pub fn series(view: &SheetView, x: Option<&str>, cols: &[String], max_points: usize) -> Result<Series> {
    let x = x.map(str::to_string).or_else(|| default_x(view));
    let xcol = match &x {
        Some(id) => Some(column(view, id)?),
        None => None,
    };
    let x_is_time = xcol.is_some_and(|(j, c)| is_time(c, view, j));
    let ys: Vec<(usize, &ColumnView)> = cols.iter().map(|id| column(view, id)).collect::<Result<_>>()?;
    // (x, value per column) for every item row with an x.
    let mut rows: Vec<(f64, Vec<Option<f64>>)> = Vec::new();
    for (n, r) in items(view) {
        let xv = match xcol {
            Some((j, c)) => match number(c, &r.cells[j], x_is_time) {
                Some(v) => v,
                None => continue,
            },
            None => (n + 1) as f64,
        };
        let vals: Vec<Option<f64>> = ys.iter().map(|(j, c)| number(c, &r.cells[*j], false)).collect();
        if vals.iter().any(|v| v.is_some()) {
            rows.push((xv, vals));
        }
    }
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    let currency = |c: &ColumnView| match &c.kind {
        ColumnKind::Money { currency } => Some(currency.code().to_string()),
        ColumnKind::Calc { currency: Some(cur), .. } => Some(cur.code().to_string()),
        _ => None,
    };
    let x_name = xcol.map_or("Row".to_string(), |(_, c)| c.name.clone());
    let points = rows.len();

    if points <= max_points || points < 2 {
        let lines = ys
            .iter()
            .enumerate()
            .map(|(k, (_, c))| Line {
                col_id: c.col_id.clone(),
                name: c.name.clone(),
                currency: currency(c),
                avg: rows.iter().map(|r| r.1[k]).collect(),
                min: None,
                max: None,
            })
            .collect();
        return Ok(Series { x: rows.iter().map(|r| r.0).collect(), x_is_time, x_name, bucket: None, points, lines });
    }

    let (lo, hi) = (rows[0].0, rows[points - 1].0);
    let width = nice_width((hi - lo) / max_points as f64, x_is_time);
    let start = (lo / width).floor() * width;
    let n_buckets = (((hi - start) / width).floor() as usize + 1).max(1);
    let mut lines: Vec<Line> = ys
        .iter()
        .map(|(_, c)| Line {
            col_id: c.col_id.clone(),
            name: c.name.clone(),
            currency: currency(c),
            avg: vec![None; n_buckets],
            min: Some(vec![None; n_buckets]),
            max: Some(vec![None; n_buckets]),
        })
        .collect();
    let mut sums = vec![vec![(0.0f64, 0usize); n_buckets]; ys.len()];
    for (xv, vals) in &rows {
        let b = (((xv - start) / width).floor() as usize).min(n_buckets - 1);
        for (k, v) in vals.iter().enumerate() {
            let Some(v) = v else { continue };
            sums[k][b].0 += v;
            sums[k][b].1 += 1;
            let line = &mut lines[k];
            let mn = &mut line.min.as_mut().unwrap()[b];
            *mn = Some(mn.map_or(*v, |m: f64| m.min(*v)));
            let mx = &mut line.max.as_mut().unwrap()[b];
            *mx = Some(mx.map_or(*v, |m: f64| m.max(*v)));
        }
    }
    for (k, line) in lines.iter_mut().enumerate() {
        for b in 0..n_buckets {
            let (s, c) = sums[k][b];
            if c > 0 {
                line.avg[b] = Some(s / c as f64);
            }
        }
    }
    let x = (0..n_buckets).map(|b| start + (b as f64 + 0.5) * width).collect();
    Ok(Series { x, x_is_time, x_name, bucket: Some(width), points, lines })
}

/// A bucket width people can read: 1, 2, 5, 10, 15, 30 s, 1, 2, 5, 10, 15, 30 min, 1, 2, 3, 6, 12 h, 1 day…
fn nice_width(raw: f64, time: bool) -> f64 {
    if time {
        const STEPS: &[f64] = &[
            0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0, 7200.0, 10800.0,
            21600.0, 43200.0, 86400.0, 172800.0, 604800.0,
        ];
        return STEPS.iter().copied().find(|s| *s >= raw).unwrap_or((raw / 86400.0).ceil() * 86400.0);
    }
    let mag = 10f64.powf(raw.max(1.0).log10().floor());
    [1.0, 2.0, 5.0, 10.0].iter().map(|m| m * mag).find(|s| *s >= raw).unwrap_or(10.0 * mag)
}

// ------------------------------------------------------------------ distribution

#[derive(Debug, Serialize, Default)]
pub struct Stats {
    pub count: usize,
    pub empty: usize,
    pub mean: f64,
    pub sd: f64,
    pub min: f64,
    pub q1: f64,
    pub median: f64,
    pub q3: f64,
    pub max: f64,
    pub sum: f64,
    /// 0 for a symmetric distribution.
    pub skewness: f64,
    /// Excess kurtosis: 0 for a normal distribution.
    pub kurtosis: f64,
    /// Values beyond 1.5 × IQR from the quartiles.
    pub outliers: usize,
    /// Fences for outliers: q1 − 1.5 IQR, q3 + 1.5 IQR.
    pub low_fence: f64,
    pub high_fence: f64,
}

#[derive(Debug, Serialize)]
pub struct Distribution {
    pub col_id: String,
    pub name: String,
    pub currency: Option<String>,
    pub is_time: bool,
    pub stats: Option<Stats>,
    /// Histogram: bin edges (one more than counts) and counts.
    pub edges: Vec<f64>,
    pub counts: Vec<usize>,
    /// The fitted normal curve at each bin's middle, scaled to counts.
    pub normal: Vec<f64>,
    /// A sentence on how normal it looks.
    pub shape: String,
    /// For text columns: (value, count), most common first.
    pub categories: Vec<(String, usize)>,
}

/// Quantile by linear interpolation between closest ranks (as Excel's QUARTILE.INC).
fn quantile(sorted: &[f64], p: f64) -> f64 {
    let h = (sorted.len() - 1) as f64 * p;
    let (lo, hi) = (h.floor() as usize, h.ceil() as usize);
    sorted[lo] + (h - lo as f64) * (sorted[hi] - sorted[lo])
}

pub fn stats(values: &[f64], empty: usize) -> Option<Stats> {
    if values.is_empty() {
        return None;
    }
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len() as f64;
    let sum: f64 = v.iter().sum();
    let mean = sum / n;
    let m2 = v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
    let m3 = v.iter().map(|x| (x - mean).powi(3)).sum::<f64>() / n;
    let m4 = v.iter().map(|x| (x - mean).powi(4)).sum::<f64>() / n;
    let sd = if v.len() > 1 { (m2 * n / (n - 1.0)).sqrt() } else { 0.0 };
    let (q1, median, q3) = (quantile(&v, 0.25), quantile(&v, 0.5), quantile(&v, 0.75));
    let iqr = q3 - q1;
    let (low_fence, high_fence) = (q1 - 1.5 * iqr, q3 + 1.5 * iqr);
    Some(Stats {
        count: v.len(),
        empty,
        mean,
        sd,
        min: v[0],
        q1,
        median,
        q3,
        max: v[v.len() - 1],
        sum,
        skewness: if m2 > 0.0 { m3 / m2.powf(1.5) } else { 0.0 },
        kurtosis: if m2 > 0.0 { m4 / (m2 * m2) - 3.0 } else { 0.0 },
        outliers: v.iter().filter(|x| **x < low_fence || **x > high_fence).count(),
        low_fence,
        high_fence,
    })
}

fn describe_shape(s: &Stats) -> String {
    if s.count < 8 {
        return "Too few values to judge the shape.".into();
    }
    if s.sd == 0.0 {
        return "Every value is the same.".into();
    }
    let side = if s.skewness > 0.0 { "right (a long tail of high values)" } else { "left (a long tail of low values)" };
    let skew = s.skewness.abs();
    let tails = if s.kurtosis > 1.0 {
        " Heavier tails than normal: more extreme values than a bell curve would give."
    } else if s.kurtosis < -1.0 {
        " Lighter tails than normal: flatter than a bell curve."
    } else {
        ""
    };
    if skew < 0.5 && s.kurtosis.abs() <= 1.0 {
        "Roughly normal: close to a bell curve.".into()
    } else if skew < 0.5 {
        format!("Roughly symmetric.{tails}")
    } else if skew < 1.0 {
        format!("Moderately skewed to the {side}.{tails}")
    } else {
        format!("Strongly skewed to the {side}.{tails}")
    }
}

pub fn distribution(view: &SheetView, col_id: &str) -> Result<Distribution> {
    let (j, c) = column(view, col_id)?;
    let time = is_time(c, view, j);
    let numeric = time || matches!(c.kind, ColumnKind::Number | ColumnKind::Money { .. } | ColumnKind::Calc { .. });
    let mut values = Vec::new();
    let mut empty = 0;
    let mut cats: HashMap<String, usize> = HashMap::new();
    for (_, r) in items(view) {
        let cell = &r.cells[j];
        match (numeric, text(cell)) {
            (_, None) => empty += 1,
            (true, Some(_)) => match number(c, cell, time) {
                Some(v) => values.push(v),
                None => empty += 1,
            },
            (false, Some(t)) => *cats.entry(t.to_string()).or_default() += 1,
        }
    }
    let currency = match &c.kind {
        ColumnKind::Money { currency } => Some(currency.code().to_string()),
        ColumnKind::Calc { currency: Some(cur), .. } => Some(cur.code().to_string()),
        _ => None,
    };
    let mut out = Distribution {
        col_id: c.col_id.clone(),
        name: c.name.clone(),
        currency,
        is_time: time,
        stats: None,
        edges: vec![],
        counts: vec![],
        normal: vec![],
        shape: String::new(),
        categories: vec![],
    };
    if !numeric {
        let mut cats: Vec<(String, usize)> = cats.into_iter().collect();
        cats.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        out.categories = cats;
        return Ok(out);
    }
    let Some(s) = stats(&values, empty) else { return Ok(out) };
    // Bin width by the Freedman–Diaconis rule, between 5 and 60 bins.
    let range = s.max - s.min;
    let bins = if range == 0.0 {
        1
    } else {
        let iqr = s.q3 - s.q1;
        let fd = if iqr > 0.0 { 2.0 * iqr / (s.count as f64).cbrt() } else { 0.0 };
        let n = if fd > 0.0 { (range / fd).ceil() as usize } else { (s.count as f64).sqrt().ceil() as usize };
        n.clamp(5, 60)
    };
    let width = if range == 0.0 { 1.0 } else { range / bins as f64 };
    out.edges = (0..=bins).map(|i| s.min + i as f64 * width).collect();
    out.counts = vec![0; bins];
    for v in &values {
        let b = (((v - s.min) / width).floor() as usize).min(bins - 1);
        out.counts[b] += 1;
    }
    if s.sd > 0.0 && !time {
        let scale = s.count as f64 * width;
        out.normal = (0..bins)
            .map(|i| {
                let x = s.min + (i as f64 + 0.5) * width;
                let z = (x - s.mean) / s.sd;
                scale * (-0.5 * z * z).exp() / (s.sd * (2.0 * std::f64::consts::PI).sqrt())
            })
            .collect();
    }
    out.shape = if time { String::new() } else { describe_shape(&s) };
    out.stats = Some(s);
    Ok(out)
}

// ------------------------------------------------------------------ groups

#[derive(Debug, Serialize)]
pub struct Aggregate {
    pub count: usize,
    pub mean: Option<f64>,
    pub min: Option<f64>,
    pub median: Option<f64>,
    pub max: Option<f64>,
    pub sum: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct Group {
    /// What the rows share: a value, or the start of a time bucket (seconds).
    pub key: String,
    pub start: Option<f64>,
    pub rows: usize,
    /// One per value column.
    pub values: Vec<Aggregate>,
}

#[derive(Debug, Serialize)]
pub struct Groups {
    pub by: String,
    pub by_time: bool,
    pub bucket: Option<f64>,
    pub columns: Vec<String>,
    pub groups: Vec<Group>,
}

/// Rows grouped by a column's values, or by time buckets when it is a time
/// column (`bucket` seconds; `None` picks one giving at most ~30 groups).
pub fn groups(view: &SheetView, by: &str, cols: &[String], bucket: Option<f64>) -> Result<Groups> {
    let (bj, bc) = column(view, by)?;
    let by_time = is_time(bc, view, bj);
    let ys: Vec<(usize, &ColumnView)> = cols.iter().map(|id| column(view, id)).collect::<Result<_>>()?;
    let mut keyed: Vec<(String, Option<f64>, Vec<Option<f64>>)> = Vec::new();
    let mut times = Vec::new();
    for (_, r) in items(view) {
        let vals: Vec<Option<f64>> = ys.iter().map(|(j, c)| number(c, &r.cells[*j], is_time(c, view, *j))).collect();
        if by_time {
            if let Some(t) = number(bc, &r.cells[bj], true) {
                times.push(t);
                keyed.push((String::new(), Some(t), vals));
            }
        } else {
            let key = text(&r.cells[bj]).map_or("(empty)".to_string(), |s| match bc.kind {
                ColumnKind::Number | ColumnKind::Money { .. } => Decimal::from_str(s).map_or(s.to_string(), |d| d.normalize().to_string()),
                _ => s.to_string(),
            });
            keyed.push((key, None, vals));
        }
    }
    let width = if by_time && !times.is_empty() {
        let lo = times.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = times.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        Some(bucket.unwrap_or_else(|| nice_width(((hi - lo) / 30.0).max(1.0), true)))
    } else {
        None
    };
    if let Some(w) = width {
        for k in keyed.iter_mut() {
            let start = (k.1.unwrap() / w).floor() * w;
            k.1 = Some(start);
            k.0 = start.to_string();
        }
    }
    // Groups in order of first appearance (time buckets in time order).
    let mut order: Vec<String> = Vec::new();
    let mut members: HashMap<String, (Option<f64>, Vec<Vec<Option<f64>>>)> = HashMap::new();
    for (key, start, vals) in keyed {
        let e = members.entry(key.clone()).or_insert_with(|| {
            order.push(key.clone());
            (start, Vec::new())
        });
        e.1.push(vals);
    }
    if by_time {
        order.sort_by(|a, b| members[a].0.unwrap_or(0.0).total_cmp(&members[b].0.unwrap_or(0.0)));
    }
    let groups = order
        .into_iter()
        .map(|key| {
            let (start, rows) = &members[&key];
            let values = (0..ys.len())
                .map(|k| {
                    let v: Vec<f64> = rows.iter().filter_map(|r| r[k]).collect();
                    match stats(&v, 0) {
                        None => Aggregate { count: 0, mean: None, min: None, median: None, max: None, sum: None },
                        Some(s) => Aggregate {
                            count: s.count,
                            mean: Some(s.mean),
                            min: Some(s.min),
                            median: Some(s.median),
                            max: Some(s.max),
                            sum: Some(s.sum),
                        },
                    }
                })
                .collect();
            Group { key, start: *start, rows: rows.len(), values }
        })
        .collect();
    Ok(Groups { by: bc.name.clone(), by_time, bucket: width, columns: ys.iter().map(|(_, c)| c.name.clone()).collect(), groups })
}

/// A group summary as a table, to keep as a sheet: the group, its row count,
/// and count, mean, min, median and max for each column.
pub fn groups_table(g: &Groups, time_label: impl Fn(f64) -> String) -> crate::import::Table {
    let mut headings = vec![g.by.clone(), "Rows".to_string()];
    for c in &g.columns {
        for a in ["mean", "min", "median", "max"] {
            headings.push(format!("{c} {a}"));
        }
    }
    let fmt = |v: Option<f64>| v.map(|x| format!("{}", (x * 1e6).round() / 1e6));
    let rows = g
        .groups
        .iter()
        .map(|gr| {
            let mut row = vec![Some(gr.start.map_or(gr.key.clone(), &time_label)), Some(gr.rows.to_string())];
            for a in &gr.values {
                row.extend([fmt(a.mean), fmt(a.min), fmt(a.median), fmt(a.max)]);
            }
            row
        })
        .collect();
    crate::import::Table { headings, rows }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quartiles_like_excel() {
        let s = stats(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0], 0).unwrap();
        assert_eq!((s.q1, s.median, s.q3), (2.75, 4.5, 6.25));
        assert_eq!(s.mean, 4.5);
        assert!((s.sd - 2.449_489_742_783_178).abs() < 1e-12);
        assert_eq!(s.outliers, 0);
        let s = stats(&[1.0, 2.0, 2.0, 3.0, 100.0], 0).unwrap();
        assert_eq!(s.outliers, 1);
        assert!(s.skewness > 1.0);
    }

    #[test]
    fn over_a_sheet() {
        use crate::{Currency, Op, Project, ProjectSettings};
        let tmp = tempfile::tempdir().unwrap();
        let mut p = Project::create(tmp.path(), ProjectSettings::new("t", Currency::new("GBP").unwrap()), "mac").unwrap();
        let mut csv = String::from("ts,mode,latency\n");
        for i in 0..100 {
            csv.push_str(&format!("{},{},{}\n", 1_789_728_000 + i, if i < 50 { "A" } else { "B" }, if i == 7 { String::new() } else { i.to_string() }));
        }
        let t = crate::import::read_csv(&csv).unwrap();
        p.apply(Op::Batch { ops: crate::import::sheet_ops(&t, "s", "Log") }).unwrap();
        let v = p.sheet_view("s", &crate::fx::RateTable::default(), chrono::NaiveDate::from_ymd_opt(2026, 10, 1).unwrap()).unwrap();
        assert_eq!(default_x(&v).as_deref(), Some("c1"));
        let s = series(&v, None, &["c3".into()], 1000).unwrap();
        assert!(s.x_is_time && s.bucket.is_none() && s.points == 99);
        let s = series(&v, None, &["c3".into()], 20).unwrap();
        assert_eq!(s.bucket, Some(5.0));
        assert_eq!(s.lines[0].avg[0], Some(2.0)); // 0..4
        assert_eq!(s.lines[0].max.as_ref().unwrap()[1], Some(9.0));
        let d = distribution(&v, "c3").unwrap();
        assert_eq!(d.stats.as_ref().unwrap().count, 99);
        assert_eq!(d.stats.as_ref().unwrap().empty, 1);
        assert_eq!(d.counts.iter().sum::<usize>(), 99);
        let d = distribution(&v, "c2").unwrap();
        assert_eq!(d.categories, vec![("A".to_string(), 50), ("B".to_string(), 50)]);
        let g = groups(&v, "c2", &["c3".into()], None).unwrap();
        assert_eq!(g.groups.len(), 2);
        assert_eq!(g.groups[1].values[0].mean, Some(74.5));
        let g = groups(&v, "c1", &["c3".into()], Some(60.0)).unwrap();
        assert!(g.by_time && g.groups.iter().map(|x| x.rows).sum::<usize>() == 100);
        let table = groups_table(&g, |t| t.to_string());
        assert_eq!(table.headings[2], "latency mean");
    }

    #[test]
    fn nice_buckets() {
        assert_eq!(nice_width(0.9, true), 1.0);
        assert_eq!(nice_width(40.0, true), 60.0);
        assert_eq!(nice_width(7.0, false), 10.0);
    }
}
