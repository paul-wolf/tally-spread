//! `tally` command line. Meant for the always-on hub (a daily `tally rates update`)
//! and for quick checks from a terminal.

use std::path::PathBuf;
use std::process::ExitCode;

use chrono::{Local, NaiveDate};
use duckdb::types::Value;
use tally_core::fx::sources::{self, fetch};
use tally_core::fx::RateSource;
use tally_core::home::{library_dir, machine_id, projects_dir, tally_home};
use tally_core::library::Library;
use tally_core::money::{format_amount, parse_amount, round, Currency};
use tally_core::Project;

const USAGE: &str = "\
tally — command line for Tally

USAGE:
  tally rates update                 download HMRC monthly, NBU and ECB rates into the library
  tally rates status                 latest date held per source
  tally rates import <source> <file> import a downloaded file (source: ecb, nbu, hmrc)
  tally convert <amount> <from> <to> [YYYY-MM-DD]
                                     convert using the library (sources: HMRC, NBU, ECB)
  tally projects                     list projects
  tally show <project> [sheet]       print a cost list with totals
  tally sql <project> <query>        run SQL against a project; each sheet is a table named after it
  tally export <project> <file> [sheet]
                                     export a sheet: .xlsx, .pdf, .csv or .json
  tally home                         print the Tally folder and this machine's id

Environment: TALLY_HOME (default ~/Tally), TALLY_MACHINE (override the machine id).";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match a.as_slice() {
        ["rates", "update"] => rates_update(),
        ["rates", "status"] => rates_status(),
        ["rates", "import", source, file] => rates_import(source, file),
        ["convert", amount, from, to] => convert(amount, from, to, None),
        ["convert", amount, from, to, date] => convert(amount, from, to, Some(date)),
        ["projects"] => projects(),
        ["show", project] => show(project, None),
        ["show", project, sheet] => show(project, Some(sheet)),
        ["sql", project, query] => sql(project, query),
        ["export", project, file] => export(project, file, None),
        ["export", project, file, sheet] => export(project, file, Some(sheet)),
        ["home"] => {
            println!("home:    {}\nmachine: {}", tally_home().display(), machine_id());
            Ok(())
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

type Res = Result<(), String>;

fn e(x: impl ToString) -> String {
    x.to_string()
}

fn today() -> NaiveDate {
    Local::now().date_naive()
}

fn library() -> Result<Library, String> {
    Library::open(library_dir(), machine_id()).map_err(e)
}

fn rates_update() -> Res {
    let lib = library()?;
    let mut failures = 0;
    let jobs: [(RateSource, fn() -> tally_core::Result<Vec<tally_core::fx::RateRecord>>); 3] = [
        (RateSource::HmrcMonthly, || fetch::hmrc_month(Local::now().date_naive())),
        (RateSource::Nbu, || fetch::nbu(None)),
        (RateSource::Ecb, fetch::ecb_daily),
    ];
    for (source, job) in jobs {
        match job() {
            Ok(records) => {
                let added = lib.add_rates(&records).map_err(e)?;
                println!("{source}: {} rates, {added} new", records.len());
            }
            Err(err) => {
                failures += 1;
                eprintln!("{source}: {err}");
            }
        }
    }
    if failures == jobs.len() {
        Err("no source could be updated".into())
    } else {
        Ok(())
    }
}

fn rates_status() -> Res {
    let rates = library()?.rates().map_err(e)?;
    for s in [RateSource::HmrcMonthly, RateSource::Nbu, RateSource::Ecb, RateSource::Manual] {
        let latest = rates.latest_date(s).map_or("none".to_string(), |d| d.to_string());
        println!("{:<13} {latest}", s.label());
    }
    println!("{} rates in total", rates.len());
    Ok(())
}

fn rates_import(source: &str, file: &str) -> Res {
    let source = match source {
        "ecb" => RateSource::Ecb,
        "nbu" => RateSource::Nbu,
        "hmrc" | "hmrc_monthly" => RateSource::HmrcMonthly,
        other => return Err(format!("unknown source {other:?} (use ecb, nbu or hmrc)")),
    };
    let text = std::fs::read_to_string(file).map_err(e)?;
    let records = sources::parse(source, &text).map_err(e)?;
    let added = library()?.add_rates(&records).map_err(e)?;
    println!("{} rates read, {added} new", records.len());
    Ok(())
}

fn convert(amount: &str, from: &str, to: &str, date: Option<&str>) -> Res {
    let (amount, _) = parse_amount(amount).map_err(e)?;
    let from = Currency::new(from).map_err(e)?;
    let to = Currency::new(to).map_err(e)?;
    let as_of = match date {
        Some(d) => NaiveDate::parse_from_str(d, "%Y-%m-%d").map_err(e)?,
        None => today(),
    };
    let rates = library()?.rates().map_err(e)?;
    let sources = [RateSource::HmrcMonthly, RateSource::Nbu, RateSource::Ecb, RateSource::Manual];
    let c = rates.convert(amount, &from, &to, as_of, &sources).map_err(e)?;
    let result = round(c.amount, &to, Default::default());
    println!("{} {from} = {} {to}", format_amount(amount, &from), format_amount(result, &to));
    for leg in c.legs {
        let (base, quote) = if leg.inverted { (&leg.to, &leg.from) } else { (&leg.from, &leg.to) };
        println!("  1 {base} = {} {quote}  ({}, {})", leg.rate, leg.source, leg.date);
    }
    Ok(())
}

fn project_dir(name: &str) -> PathBuf {
    let p = PathBuf::from(name);
    if p.join("project.json").exists() {
        p
    } else {
        projects_dir().join(name)
    }
}

fn open(name: &str) -> Result<Project, String> {
    let mut p = Project::open(project_dir(name), &machine_id()).map_err(|err| format!("{name}: {err}"))?;
    let (fns, problems) = library()?.functions().map_err(e)?;
    for pr in problems {
        eprintln!("warning: {pr}");
    }
    p.set_functions(fns);
    Ok(p)
}

fn projects() -> Res {
    let dir = projects_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        println!("no projects in {}", dir.display());
        return Ok(());
    };
    for entry in entries.flatten() {
        if entry.path().join("project.json").exists() {
            let p = Project::open(entry.path(), &machine_id()).map_err(e)?;
            println!("{:<24} {}", entry.file_name().to_string_lossy(), p.settings().name);
        }
    }
    Ok(())
}

fn sheet_view(project: &str, sheet: Option<&str>) -> Result<tally_core::sheet::SheetView, String> {
    let p = open(project)?;
    let sheets = p.sheets(false).map_err(e)?;
    let chosen = match sheet {
        Some(name) => sheets
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(name) || s.sheet_id == name)
            .ok_or_else(|| format!("no sheet {name:?}"))?,
        None => sheets.first().ok_or("project has no sheets")?,
    };
    let rates = library()?.rates().map_err(e)?;
    p.sheet_view(&chosen.sheet_id, &rates, today()).map_err(e)
}

fn export(project: &str, file: &str, sheet: Option<&str>) -> Res {
    let v = sheet_view(project, sheet)?;
    let path = std::path::Path::new(file);
    match path.extension().and_then(|x| x.to_str()).unwrap_or("").to_ascii_lowercase().as_str() {
        "xlsx" => tally_core::export::export_xlsx(&v, path).map_err(e)?,
        "csv" => std::fs::write(path, tally_core::formats::to_csv(&v).map_err(e)?).map_err(e)?,
        "json" => std::fs::write(path, serde_json::to_string_pretty(&tally_core::formats::to_json(&v, project)).map_err(e)?).map_err(e)?,
        "pdf" => std::fs::write(path, tally_core::formats::to_pdf(&v, project).map_err(e)?).map_err(e)?,
        other => return Err(format!("can't export to .{other}: use .xlsx, .pdf, .csv or .json")),
    }
    println!("wrote {file}");
    Ok(())
}

fn show(project: &str, sheet: Option<&str>) -> Res {
    use tally_core::RowKind;
    let v = sheet_view(project, sheet)?;
    println!("{}  (rates as of {})\n", v.name, v.as_of);
    let width = |j: usize| if j == 0 { 30 } else { 14 };
    let fit = |s: &str, w: usize| s.chars().take(w).collect::<String>();
    let head: Vec<String> = v.columns.iter().enumerate().map(|(j, c)| format!("{:<w$}", fit(&format!("{} {}", c.col_id, c.name), width(j)), w = width(j))).collect();
    println!("{}", head.join(" "));
    for r in &v.rows {
        let cells: Vec<String> = r
            .cells
            .iter()
            .enumerate()
            .map(|(j, c)| {
                let text = c.raw.clone().or_else(|| c.value.clone()).or_else(|| c.error.as_ref().map(|_| "!".to_string())).unwrap_or_default();
                let text = if r.kind == RowKind::Section { text.to_uppercase() } else { text };
                format!("{:<w$}", fit(&text, width(j)), w = width(j))
            })
            .collect();
        println!("{}", cells.join(" "));
    }
    let totals: Vec<String> = v
        .totals
        .iter()
        .enumerate()
        .map(|(j, t)| {
            let s = match t {
                Some(t) if t.complete => t.amount.clone(),
                Some(t) => format!("{}*", t.amount),
                None if j == 0 => "TOTAL".into(),
                None => String::new(),
            };
            format!("{:<w$}", s, w = width(j))
        })
        .collect();
    println!("{}", totals.join(" "));
    for leg in &v.rates_used {
        println!("  1 {} = {} {}  ({}, {})", leg.from, leg.rate, leg.to, leg.source, leg.date);
    }
    for w in &v.warnings {
        println!("  * {w}");
    }
    Ok(())
}

fn sql(project: &str, query: &str) -> Res {
    let p = open(project)?;
    // Each sheet becomes a table named after it, with calculated values included.
    let rates = library()?.rates().map_err(e)?;
    p.materialize(&rates, today()).map_err(e)?;
    let mut stmt = p.connection().prepare(query).map_err(e)?;
    let mut out: Vec<Vec<String>> = Vec::new();
    {
        let mut rows = stmt.query([]).map_err(e)?;
        while let Some(row) = rows.next().map_err(e)? {
            let n = row.as_ref().column_count();
            let mut line = Vec::with_capacity(n);
            for i in 0..n {
                let v: Value = row.get(i).map_err(e)?;
                line.push(fmt_value(v));
            }
            out.push(line);
        }
    }
    println!("{}", stmt.column_names().join("\t"));
    for line in out {
        println!("{}", line.join("\t"));
    }
    Ok(())
}

fn fmt_value(v: Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Boolean(b) => b.to_string(),
        Value::TinyInt(n) => n.to_string(),
        Value::SmallInt(n) => n.to_string(),
        Value::Int(n) => n.to_string(),
        Value::BigInt(n) => n.to_string(),
        Value::HugeInt(n) => n.to_string(),
        Value::UTinyInt(n) => n.to_string(),
        Value::USmallInt(n) => n.to_string(),
        Value::UInt(n) => n.to_string(),
        Value::UBigInt(n) => n.to_string(),
        Value::Float(n) => n.to_string(),
        Value::Double(n) => n.to_string(),
        Value::Decimal(d) => d.to_string(),
        Value::Text(s) => s,
        other => format!("{other:?}"),
    }
}
