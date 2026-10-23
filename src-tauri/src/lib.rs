//! Tauri shell: holds the open project and the rate library, and exposes them to
//! the UI as commands. All math happens in `tally-core`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{Local, NaiveDate};
use serde::Serialize;
use tally_core::fx::sources::{self, fetch};
use tally_core::fx::{RateRecord, RateSource, RateTable};
use tally_core::home::{machine_id, tally_home};
use tally_core::library::Library;
use tally_core::project::{read_settings, SheetInfo, FORMAT};
use tally_core::sheet::SheetView;
use tally_core::formula::{self, UserFunction, Value};
use tally_core::templates;
use tally_core::{Currency, Op, Project, ProjectSettings};
use tauri::State;

type CmdResult<T> = Result<T, String>;

fn err(e: impl ToString) -> String {
    e.to_string()
}

struct Inner {
    home: PathBuf,
    machine: String,
    library: Library,
    rates: RateTable,
    project: Project,
    project_dir_name: String,
}

pub struct AppState(Mutex<Inner>);

#[derive(Serialize)]
pub struct ProjectSummary {
    pub dir_name: String,
    pub name: String,
}

#[derive(Serialize)]
pub struct RateStatus {
    pub source: RateSource,
    pub label: String,
    pub latest: Option<NaiveDate>,
}

#[derive(Serialize)]
pub struct Snapshot {
    pub home: String,
    pub machine: String,
    pub projects: Vec<ProjectSummary>,
    pub project: String,
    pub settings: ProjectSettings,
    pub sheets: Vec<SheetInfo>,
    pub sheet: Option<SheetView>,
    pub can_undo: bool,
    pub can_redo: bool,
    pub rate_status: Vec<RateStatus>,
    pub functions: Vec<UserFunction>,
    pub notices: Vec<String>,
}

#[derive(Serialize)]
pub struct RefreshReport {
    pub source: RateSource,
    pub ok: bool,
    pub added: usize,
    pub message: String,
}

// ------------------------------------------------------------------ setup

fn projects_dir(home: &Path) -> PathBuf {
    home.join("projects")
}

fn list_projects(home: &Path) -> Vec<ProjectSummary> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(projects_dir(home)) {
        for e in entries.flatten() {
            let path = e.path();
            // Projects from an earlier version of Tally cannot be opened; skip them.
            let Ok(settings) = read_settings(&path) else { continue };
            // Format 2 is converted when opened; older ones are skipped.
            if settings.format < FORMAT - 1 {
                continue;
            }
            out.push(ProjectSummary { dir_name: e.file_name().to_string_lossy().into_owned(), name: settings.name });
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

fn slug(name: &str) -> String {
    let s: String = name
        .trim()
        .chars()
        .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    let s = s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    if s.is_empty() { "project".into() } else { s }
}

/// Settings for a new project: ECB rates first, NBU for hryvnia (which ECB does
/// not publish), UK as tax home. Change them per project in `project.json`.
fn default_settings(name: &str) -> ProjectSettings {
    let mut s = ProjectSettings::new(name, Currency::new("GBP").unwrap());
    s.rate_sources = vec![RateSource::Ecb, RateSource::Nbu, RateSource::HmrcMonthly, RateSource::Manual];
    s.tax_home = Some("GB".into());
    s
}

fn create_project_in(home: &Path, name: &str, machine: &str) -> Result<(Project, String), String> {
    let base = slug(name);
    let mut dir_name = base.clone();
    let mut n = 2;
    while projects_dir(home).join(&dir_name).exists() {
        dir_name = format!("{base}-{n}");
        n += 1;
    }
    let dir = projects_dir(home).join(&dir_name);
    let mut p = Project::create(&dir, default_settings(name), machine).map_err(err)?;
    let ops = templates::sheet_ops("spending", &Op::new_id(), "Spending");
    p.apply(Op::Batch { ops }).map_err(err)?;
    // Reopen so the first sheet is not on the undo stack.
    let p = Project::open(&dir, machine).map_err(err)?;
    Ok((p, dir_name))
}

fn init() -> Result<Inner, String> {
    let home = tally_home();
    let machine = machine_id();
    fs::create_dir_all(projects_dir(&home)).map_err(err)?;
    let library = Library::open(home.join("library"), &machine).map_err(err)?;
    let rates = library.rates().map_err(err)?;

    let last = fs::read_to_string(home.join(".last-project")).ok().map(|s| s.trim().to_string());
    let existing = list_projects(&home);
    let (project, project_dir_name) = match last
        .filter(|d| existing.iter().any(|p| &p.dir_name == d))
        .or_else(|| existing.first().map(|p| p.dir_name.clone()))
    {
        Some(dir) => (Project::open(projects_dir(&home).join(&dir), &machine).map_err(err)?, dir),
        None => create_project_in(&home, "My project", &machine)?,
    };
    let mut inner = Inner { home, machine, library, rates, project, project_dir_name };
    inner.load_functions()?;
    Ok(inner)
}

// ------------------------------------------------------------------ snapshot

fn today() -> NaiveDate {
    Local::now().date_naive()
}

impl Inner {
    fn snapshot(&self, sheet_id: Option<&str>, notices: Vec<String>) -> CmdResult<Snapshot> {
        let sheets = self.project.sheets(false).map_err(err)?;
        let chosen = sheet_id
            .filter(|id| sheets.iter().any(|s| s.sheet_id == *id))
            .or_else(|| sheets.first().map(|s| s.sheet_id.as_str()));
        let sheet = match chosen {
            Some(id) => Some(self.project.sheet_view(id, &self.rates, today()).map_err(err)?),
            None => None,
        };
        let mut notices = notices;
        notices.extend(self.project.replay_warnings().iter().cloned());
        Ok(Snapshot {
            home: self.home.display().to_string(),
            machine: self.machine.clone(),
            projects: list_projects(&self.home),
            project: self.project_dir_name.clone(),
            settings: self.project.settings().clone(),
            sheets,
            sheet,
            can_undo: self.project.can_undo(),
            can_redo: self.project.can_redo(),
            functions: self.project.functions().list().into_iter().cloned().collect(),
            rate_status: [RateSource::HmrcMonthly, RateSource::Nbu, RateSource::Ecb]
                .into_iter()
                .map(|s| RateStatus { source: s, label: s.label().into(), latest: self.rates.latest_date(s) })
                .collect(),
            notices,
        })
    }

    fn reload_rates(&mut self) -> CmdResult<()> {
        self.rates = self.library.rates().map_err(err)?;
        Ok(())
    }

    /// Give the open project the library's user functions.
    fn load_functions(&mut self) -> CmdResult<Vec<String>> {
        let (fns, problems) = self.library.functions().map_err(err)?;
        self.project.set_functions(fns);
        Ok(problems)
    }
}

// ------------------------------------------------------------------ commands

#[tauri::command]
fn snapshot(state: State<AppState>, sheet_id: Option<String>) -> CmdResult<Snapshot> {
    let inner = state.0.lock().map_err(err)?;
    inner.snapshot(sheet_id.as_deref(), vec![])
}

#[tauri::command]
fn apply(state: State<AppState>, op: Op, sheet_id: Option<String>) -> CmdResult<Snapshot> {
    let mut inner = state.0.lock().map_err(err)?;
    inner.project.apply(op).map_err(err)?;
    inner.snapshot(sheet_id.as_deref(), vec![])
}

#[tauri::command]
fn undo(state: State<AppState>, sheet_id: Option<String>) -> CmdResult<Snapshot> {
    let mut inner = state.0.lock().map_err(err)?;
    inner.project.undo().map_err(err)?;
    inner.snapshot(sheet_id.as_deref(), vec![])
}

#[tauri::command]
fn redo(state: State<AppState>, sheet_id: Option<String>) -> CmdResult<Snapshot> {
    let mut inner = state.0.lock().map_err(err)?;
    inner.project.redo().map_err(err)?;
    inner.snapshot(sheet_id.as_deref(), vec![])
}

/// Pick up changes synced from other machines (project log and rate library).
#[tauri::command]
fn sync(state: State<AppState>, sheet_id: Option<String>) -> CmdResult<Option<Snapshot>> {
    let mut inner = state.0.lock().map_err(err)?;
    let before = inner.rates.len();
    inner.reload_rates()?;
    let before_fns = inner.project.functions().list().len();
    inner.load_functions()?;
    let fns_changed = inner.project.functions().list().len() != before_fns;
    let changed = inner.project.refresh().map_err(err)? || inner.rates.len() != before || fns_changed;
    if changed {
        Ok(Some(inner.snapshot(sheet_id.as_deref(), vec![])?))
    } else {
        Ok(None)
    }
}

#[tauri::command]
fn open_project(state: State<AppState>, dir_name: String) -> CmdResult<Snapshot> {
    let mut inner = state.0.lock().map_err(err)?;
    let dir = projects_dir(&inner.home).join(&dir_name);
    inner.project = Project::open(dir, &inner.machine).map_err(err)?;
    inner.load_functions()?;
    inner.project_dir_name = dir_name.clone();
    let _ = fs::write(inner.home.join(".last-project"), &dir_name);
    inner.snapshot(None, vec![])
}

#[tauri::command]
fn create_project(state: State<AppState>, name: String) -> CmdResult<Snapshot> {
    let mut inner = state.0.lock().map_err(err)?;
    let (project, dir_name) = create_project_in(&inner.home, &name, &inner.machine)?;
    inner.project = project;
    inner.load_functions()?;
    inner.project_dir_name = dir_name.clone();
    let _ = fs::write(inner.home.join(".last-project"), &dir_name);
    inner.snapshot(None, vec![])
}

#[tauri::command]
fn save_settings(state: State<AppState>, settings: ProjectSettings, sheet_id: Option<String>) -> CmdResult<Snapshot> {
    let mut inner = state.0.lock().map_err(err)?;
    inner.project.save_settings(settings).map_err(err)?;
    inner.snapshot(sheet_id.as_deref(), vec![])
}

/// Download the latest official rates. Each source is tried on its own; one
/// failing does not stop the others.
#[tauri::command]
async fn refresh_rates(state: State<'_, AppState>) -> CmdResult<Vec<RefreshReport>> {
    let fetched: Vec<(RateSource, Result<Vec<RateRecord>, String>)> = tauri::async_runtime::spawn_blocking(|| {
        let day = today();
        vec![
            (RateSource::HmrcMonthly, fetch::hmrc_month(day).map_err(err)),
            (RateSource::Nbu, fetch::nbu(None).map_err(err)),
            (RateSource::Ecb, fetch::ecb_daily().map_err(err)),
        ]
    })
    .await
    .map_err(err)?;

    let mut inner = state.0.lock().map_err(err)?;
    let mut reports = Vec::new();
    for (source, result) in fetched {
        reports.push(match result {
            Ok(records) => match inner.library.add_rates(&records) {
                Ok(added) => RefreshReport { source, ok: true, added, message: format!("{} rates", records.len()) },
                Err(e) => RefreshReport { source, ok: false, added: 0, message: e.to_string() },
            },
            Err(message) => RefreshReport { source, ok: false, added: 0, message },
        });
    }
    inner.reload_rates()?;
    Ok(reports)
}

/// Import a rates file downloaded by hand (ECB XML, NBU JSON or HMRC CSV).
#[tauri::command]
fn import_rates(state: State<AppState>, source: RateSource, path: String) -> CmdResult<usize> {
    let text = fs::read_to_string(&path).map_err(err)?;
    let records = sources::parse(source, &text).map_err(err)?;
    let mut inner = state.0.lock().map_err(err)?;
    let added = inner.library.add_rates(&records).map_err(err)?;
    inner.reload_rates()?;
    Ok(added)
}

/// Add a rate by hand: `1 base = rate quote` from `date`.
#[tauri::command]
fn add_manual_rate(state: State<AppState>, base: String, quote: String, rate: String, date: NaiveDate) -> CmdResult<usize> {
    let (rate, _) = tally_core::money::parse_amount(&rate).map_err(err)?;
    let rec = RateRecord {
        date,
        valid_to: None,
        base: Currency::new(&base).map_err(err)?,
        quote: Currency::new(&quote).map_err(err)?,
        rate,
        source: RateSource::Manual,
    };
    let mut inner = state.0.lock().map_err(err)?;
    let added = inner.library.add_rates(&[rec]).map_err(err)?;
    inner.reload_rates()?;
    Ok(added)
}

/// Create a sheet from a template ("blank" or "spending").
#[tauri::command]
fn create_sheet(state: State<AppState>, name: String, template: String) -> CmdResult<Snapshot> {
    let mut inner = state.0.lock().map_err(err)?;
    let id = Op::new_id();
    inner.project.apply(Op::Batch { ops: templates::sheet_ops(&template, &id, &name) }).map_err(err)?;
    inner.snapshot(Some(&id), vec![])
}

/// Try a column formula as it is typed: its result on the first rows, or the problem.
#[tauri::command]
fn preview_formula(
    state: State<AppState>,
    sheet_id: String,
    col_id: Option<String>,
    formula: String,
    currency: Option<String>,
) -> CmdResult<Vec<String>> {
    let inner = state.0.lock().map_err(err)?;
    let currency = currency.map(|c| Currency::new(&c)).transpose().map_err(err)?;
    inner.project.preview_formula(&sheet_id, col_id.as_deref(), &formula, currency, &inner.rates, today()).map_err(err)
}

/// Save a user function (shared by every sheet and project); `old_name` renames one.
#[tauri::command]
fn save_function(state: State<AppState>, function: UserFunction, old_name: Option<String>, sheet_id: Option<String>) -> CmdResult<Snapshot> {
    let mut inner = state.0.lock().map_err(err)?;
    inner.library.save_function(function, old_name.as_deref()).map_err(err)?;
    let problems = inner.load_functions()?;
    inner.snapshot(sheet_id.as_deref(), problems)
}

#[tauri::command]
fn delete_function(state: State<AppState>, name: String, sheet_id: Option<String>) -> CmdResult<Snapshot> {
    let mut inner = state.0.lock().map_err(err)?;
    inner.library.delete_function(&name).map_err(err)?;
    let problems = inner.load_functions()?;
    inner.snapshot(sheet_id.as_deref(), problems)
}

/// Try an expression such as `vat(100)` with a draft of a function in place.
#[tauri::command]
fn try_function(state: State<AppState>, function: UserFunction, expression: String) -> CmdResult<String> {
    let inner = state.0.lock().map_err(err)?;
    formula::compile_function(&function)?;
    let mut all: Vec<UserFunction> =
        inner.project.functions().list().into_iter().filter(|f| f.name != function.name).cloned().collect();
    all.push(function);
    let (fns, _) = formula::Functions::new(all);
    Ok(match formula::eval_standalone(&expression, &fns) {
        Value::Empty => "(empty)".into(),
        Value::Number(n) => n.normalize().to_string(),
        Value::Money(n, c) => format!("{} {c}", n.normalize()),
        Value::Text(t) => t,
        Value::Currency(c) => c.to_string(),
        Value::Error(e) => return Err(e),
    })
}

#[tauri::command]
fn export_xlsx(state: State<AppState>, sheet_id: String, path: String) -> CmdResult<()> {
    let inner = state.0.lock().map_err(err)?;
    let view = inner.project.sheet_view(&sheet_id, &inner.rates, today()).map_err(err)?;
    tally_core::export::export_xlsx(&view, Path::new(&path)).map_err(err)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let inner = init().expect("could not open the Tally folder");
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(AppState(Mutex::new(inner)))
        .invoke_handler(tauri::generate_handler![
            snapshot,
            apply,
            undo,
            redo,
            sync,
            open_project,
            create_project,
            save_settings,
            refresh_rates,
            import_rates,
            add_manual_rate,
            create_sheet,
            preview_formula,
            save_function,
            delete_function,
            try_function,
            export_xlsx
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tally");
}
