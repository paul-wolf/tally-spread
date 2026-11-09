//! Tauri shell: holds the open projects, the open sheet tabs and the rate library,
//! and exposes them to the UI as commands. All math happens in `tally-core`.
//!
//! There is no "current project" for the person to choose: tabs can show sheets from
//! any project, and the project of the tab being shown is the active one (undo, redo
//! and edits act on it). New sheets go to Unfiled, a hidden project, unless started
//! inside a project; Move to project files them later.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{Local, NaiveDate};
use serde::{Deserialize, Serialize};
use tally_core::analysis;
use tally_core::fx::sources::{self, fetch};
use tally_core::fx::{RateRecord, RateSource, RateTable};
use tally_core::home::{machine_id, open_project_dir, take_open_requests, tally_home, UNFILED};
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

thread_local! {
    /// The last panic on this thread, as recorded by the panic hook, for `guard`.
    static LAST_PANIC: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Run a command's body so that a bug in it fails only that command, shown to the
/// person as an error, instead of closing the app. (A panic unwinding out of a
/// synchronous command would cross WebKit's Objective-C callback, which aborts.)
fn guard<T>(body: impl FnOnce() -> CmdResult<T>) -> CmdResult<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).unwrap_or_else(|_| {
        let what = LAST_PANIC.with(|p| p.borrow_mut().take()).unwrap_or_else(|| "unknown error".into());
        Err(format!("Tally hit a bug and stopped this action: {what}"))
    })
}

/// File in Tally's home folder that collects panics, with backtraces, for bug reports.
const PANIC_LOG: &str = ".panic.log";

/// Record each panic for `guard` (message and source location) and in the panic log,
/// then report it as usual.
fn install_panic_hook(home: PathBuf) {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let what = info.to_string().replace('\n', " ");
        LAST_PANIC.with(|p| *p.borrow_mut() = Some(what.clone()));
        let entry = format!(
            "{} {} {what}\n{}\n\n",
            Local::now().to_rfc3339(),
            env!("CARGO_PKG_VERSION"),
            std::backtrace::Backtrace::force_capture()
        );
        let log = fs::OpenOptions::new().create(true).append(true).open(home.join(PANIC_LOG));
        if let Ok(mut f) = log {
            let _ = std::io::Write::write_all(&mut f, entry.as_bytes());
        }
        default(info);
    }));
}

/// Open tabs, kept per machine in Tally's home folder.
const TABS_FILE: &str = ".tabs.json";
/// When each sheet was last shown on this machine ("project/sheet" → Unix ms).
const OPENED_FILE: &str = ".opened.json";
/// Most unpinned tabs kept open; opening more closes the least recently used.
const DEFAULT_MAX_TABS: usize = 12;

/// An open sheet tab: a sheet in a project (by folder name).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Tab {
    project: String,
    sheet_id: String,
}

#[derive(Default, Serialize, Deserialize)]
struct TabsFile {
    tabs: Vec<Tab>,
    current: Option<Tab>,
    #[serde(default)]
    pinned: Vec<Tab>,
    #[serde(default)]
    max_tabs: Option<usize>,
}

fn tab_key(t: &Tab) -> String {
    format!("{}/{}", t.project, t.sheet_id)
}

struct Inner {
    home: PathBuf,
    machine: String,
    library: Library,
    rates: RateTable,
    /// The project of the tab being shown.
    project: Project,
    project_dir_name: String,
    /// Other open projects (those of the other tabs), by folder name.
    others: HashMap<String, Project>,
    tabs: Vec<Tab>,
    current: Option<Tab>,
    /// Pinned tabs are never closed to make room.
    pinned: Vec<Tab>,
    max_tabs: usize,
    /// Last shown on this machine, by [`tab_key`].
    opened: HashMap<String, i64>,
    /// Notices from tab housekeeping, shown with the next snapshot.
    pending: Vec<String>,
}

pub struct AppState(Mutex<Inner>);

impl AppState {
    /// The app's state. A command that panicked while holding it (reported as an error
    /// by `guard`) leaves the lock poisoned; carry on with the state as it was left
    /// rather than failing every later command.
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|poisoned| {
            self.0.clear_poison();
            poisoned.into_inner()
        })
    }
}

#[derive(Serialize)]
pub struct ProjectSummary {
    pub dir_name: String,
    pub name: String,
    /// The project's colour as a hue in degrees; see [`PROJECT_HUES`].
    pub hue: Option<u16>,
}

/// Project colours: hues (OKLCH degrees) in the order they are handed out, each far
/// from the ones before it, so the first few projects are easy to tell apart.
const PROJECT_HUES: [u16; 8] = [25, 230, 135, 310, 70, 185, 270, 350];

/// The colour for a new project: the one fewest projects have, earliest in
/// [`PROJECT_HUES`] on a tie.
fn pick_hue(used: &[u16]) -> u16 {
    *PROJECT_HUES
        .iter()
        .min_by_key(|h| used.iter().filter(|u| u == h).count())
        .expect("PROJECT_HUES is not empty")
}

/// Give every project made before colours existed a colour of its own, in folder
/// order, saved in its `project.json`. Runs at start-up, before any project is open.
fn assign_missing_colours(home: &Path) {
    let Ok(entries) = fs::read_dir(projects_dir(home)) else { return };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('_'))
        .map(|e| e.path())
        .collect();
    dirs.sort();
    let found: Vec<(PathBuf, ProjectSettings)> = dirs.into_iter().filter_map(|d| read_settings(&d).ok().map(|s| (d, s))).collect();
    let mut used: Vec<u16> = found.iter().filter_map(|(_, s)| s.color).collect();
    for (dir, mut settings) in found {
        if settings.color.is_some() {
            continue;
        }
        let hue = pick_hue(&used);
        settings.color = Some(hue);
        used.push(hue);
        // Only the colour changes; the format stays as it was until the project is opened.
        if let Ok(json) = serde_json::to_string_pretty(&settings) {
            let _ = fs::write(dir.join("project.json"), json);
        }
    }
}

/// A tab as the UI shows it.
#[derive(Clone, Serialize)]
pub struct TabInfo {
    pub project: String,
    pub project_name: String,
    pub sheet_id: String,
    pub name: String,
    pub unfiled: bool,
    pub pinned: bool,
}

/// A sheet as Home and Go to Sheet list it.
#[derive(Serialize)]
pub struct SheetEntry {
    pub project: String,
    pub project_name: String,
    pub unfiled: bool,
    pub sheet_id: String,
    pub name: String,
    pub archived: bool,
    pub rows: usize,
    pub columns: usize,
    /// "money" (has amounts), "log" (has dates or times) or "table".
    pub kind: String,
    /// The sheet's first total, if any column is totalled.
    pub total: Option<String>,
    pub total_currency: Option<Currency>,
    /// Last changed on any machine, Unix ms.
    pub edited: Option<i64>,
    /// Last shown on this machine, Unix ms.
    pub opened: Option<i64>,
    pub open: bool,
    pub pinned: bool,
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
    /// Projects to file sheets in (Unfiled is not among them).
    pub projects: Vec<ProjectSummary>,
    /// Folder name of the active project: the shown tab's.
    pub project: String,
    /// Folder name of Unfiled.
    pub unfiled: String,
    pub tabs: Vec<TabInfo>,
    pub settings: ProjectSettings,
    pub sheets: Vec<SheetInfo>,
    pub sheet: Option<SheetView>,
    pub can_undo: bool,
    pub can_redo: bool,
    pub rate_status: Vec<RateStatus>,
    pub functions: Vec<UserFunction>,
    pub notices: Vec<String>,
    /// Show this snapshot's sheet even if Home is shown: it was asked for from
    /// outside the app (the `tally import` command).
    pub focus: bool,
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
            if e.file_name().to_string_lossy().starts_with('_') {
                continue; // Unfiled and other hidden folders
            }
            let path = e.path();
            // Projects from an earlier version of Tally cannot be opened; skip them.
            let Ok(settings) = read_settings(&path) else { continue };
            // Format 2 is converted when opened; older ones are skipped.
            if settings.format < FORMAT - 1 {
                continue;
            }
            out.push(ProjectSummary { dir_name: e.file_name().to_string_lossy().into_owned(), name: settings.name, hue: settings.color });
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

fn create_project_in(home: &Path, name: &str, machine: &str, starter: bool) -> Result<(Project, String), String> {
    let base = slug(name);
    let mut dir_name = base.clone();
    let mut n = 2;
    while projects_dir(home).join(&dir_name).exists() {
        dir_name = format!("{base}-{n}");
        n += 1;
    }
    let dir = projects_dir(home).join(&dir_name);
    let mut settings = default_settings(name);
    let used: Vec<u16> = list_projects(home).iter().filter_map(|p| p.hue).collect();
    settings.color = Some(pick_hue(&used));
    let mut p = Project::create(&dir, settings, machine).map_err(err)?;
    if starter {
        let ops = templates::sheet_ops("spending", &Op::new_id(), "Spending");
        p.apply(Op::Batch { ops }).map_err(err)?;
        // Reopen so the first sheet is not on the undo stack.
        p = Project::open(&dir, machine).map_err(err)?;
    }
    Ok((p, dir_name))
}

/// Open a project by folder name, creating Unfiled the first time it is needed.
fn open_dir(home: &Path, machine: &str, dir_name: &str) -> Result<Project, String> {
    open_project_dir(&projects_dir(home), dir_name, machine).map_err(err)
}

fn visible(p: &Project, sheet_id: &str) -> bool {
    p.sheet(sheet_id).is_ok_and(|s| !s.archived)
}

fn visible_ids(p: &Project) -> Vec<String> {
    p.sheets(false).map(|v| v.into_iter().map(|s| s.sheet_id).collect()).unwrap_or_default()
}

fn init() -> Result<Inner, String> {
    init_at(tally_home(), machine_id())
}

fn init_at(home: PathBuf, machine: String) -> Result<Inner, String> {
    fs::create_dir_all(projects_dir(&home)).map_err(err)?;
    assign_missing_colours(&home);
    let library = Library::open(home.join("library"), &machine).map_err(err)?;
    let rates = library.rates().map_err(err)?;

    let saved: Option<TabsFile> = fs::read(home.join(TABS_FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok());
    let project = open_dir(&home, &machine, UNFILED)?;
    let mut inner = Inner {
        home,
        machine,
        library,
        rates,
        project,
        project_dir_name: UNFILED.into(),
        others: HashMap::new(),
        tabs: vec![],
        current: None,
        pinned: vec![],
        max_tabs: DEFAULT_MAX_TABS,
        opened: HashMap::new(),
        pending: vec![],
    };
    inner.opened = fs::read(inner.home.join(OPENED_FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    inner.load_functions()?;
    match saved {
        Some(f) => {
            inner.tabs = f.tabs;
            inner.current = f.current;
            inner.pinned = f.pinned;
            inner.max_tabs = f.max_tabs.unwrap_or(DEFAULT_MAX_TABS).max(1);
        }
        None => {
            // First run with tabs: open the last project's sheets, so nothing seems to
            // have gone. With no projects at all, start with a spending list in Unfiled.
            let last = fs::read_to_string(inner.home.join(".last-project")).ok().map(|s| s.trim().to_string());
            let existing = list_projects(&inner.home);
            match last.filter(|d| existing.iter().any(|p| &p.dir_name == d)).or_else(|| existing.first().map(|p| p.dir_name.clone())) {
                Some(dir) => {
                    inner.activate(&dir)?;
                    for id in visible_ids(&inner.project) {
                        inner.tabs.push(Tab { project: dir.clone(), sheet_id: id });
                    }
                }
                None => {
                    let id = Op::new_id();
                    inner.project.apply(Op::Batch { ops: templates::sheet_ops("spending", &id, "Spending") }).map_err(err)?;
                    inner.project = open_dir(&inner.home, &inner.machine, UNFILED)?; // keep the first sheet off the undo stack
                    inner.load_functions()?;
                    inner.tabs.push(Tab { project: UNFILED.into(), sheet_id: id });
                }
            }
            inner.current = inner.tabs.first().cloned();
        }
    }
    inner.load_tab_projects();
    // Files imported with `tally import` while the app wasn't running.
    if let Some(t) = inner.open_requested() {
        inner.current = Some(t);
    }
    if let Some(t) = inner.current.clone().or_else(|| inner.tabs.first().cloned()) {
        inner.activate(&t.project)?;
    }
    Ok(inner)
}

// ------------------------------------------------------------------ snapshot

fn today() -> NaiveDate {
    Local::now().date_naive()
}

impl Inner {
    /// What the UI shows. `sheet_id` must be a visible sheet of the active project
    /// (see [`Inner::respond`], which picks it).
    fn snapshot(&self, sheet_id: Option<&str>, notices: Vec<String>) -> CmdResult<Snapshot> {
        let sheets = self.project.sheets(false).map_err(err)?;
        let chosen = sheet_id.filter(|id| sheets.iter().any(|s| s.sheet_id == *id));
        let tabs = self
            .tabs
            .iter()
            .filter_map(|t| {
                let p = self.project_ref(&t.project)?;
                let s = p.sheet(&t.sheet_id).ok().filter(|s| !s.archived)?;
                Some(TabInfo {
                    project: t.project.clone(),
                    project_name: p.settings().name.clone(),
                    sheet_id: t.sheet_id.clone(),
                    name: s.name,
                    unfiled: t.project == UNFILED,
                    pinned: self.pinned.contains(t),
                })
            })
            .collect();
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
            unfiled: UNFILED.into(),
            tabs,
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
            focus: false,
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

    fn project_ref(&self, dir: &str) -> Option<&Project> {
        if dir == self.project_dir_name {
            Some(&self.project)
        } else {
            self.others.get(dir)
        }
    }

    fn project_mut(&mut self, dir: &str) -> Option<&mut Project> {
        if dir == self.project_dir_name {
            Some(&mut self.project)
        } else {
            self.others.get_mut(dir)
        }
    }

    /// Open the sheets that `tally import` asked for, as tabs after the current one.
    /// Returns the tab to show (the first sheet of the latest request); a notice
    /// says what opened.
    fn open_requested(&mut self) -> Option<Tab> {
        let mut show = None;
        let mut names = Vec::new();
        for req in take_open_requests(&self.home, &self.machine) {
            // The command wrote to the project's log behind the app's back.
            let ready = match self.project_mut(&req.project) {
                Some(p) => p.refresh().is_ok(),
                None => match open_dir(&self.home, &self.machine, &req.project) {
                    Ok(p) => {
                        self.others.insert(req.project.clone(), p);
                        true
                    }
                    Err(_) => false,
                },
            };
            let Some(p) = self.project_ref(&req.project).filter(|_| ready) else { continue };
            let ids: Vec<String> = req.sheet_ids.into_iter().filter(|id| visible(p, id)).collect();
            names.extend(ids.iter().filter_map(|id| p.sheet(id).ok()).map(|s| s.name));
            for id in ids.iter().rev() {
                self.add_tab(Tab { project: req.project.clone(), sheet_id: id.clone() });
            }
            if let Some(first) = ids.first() {
                show = Some(Tab { project: req.project.clone(), sheet_id: first.clone() });
            }
        }
        match names.as_slice() {
            [] => {}
            [one] => self.pending.push(format!("Opened “{one}” from the command line.")),
            many => self.pending.push(format!("Opened {} sheets from the command line.", many.len())),
        }
        if let Some(t) = &show {
            self.mark_opened(t);
        }
        show
    }

    /// Make `dir` the active project, keeping the previous one open.
    fn activate(&mut self, dir: &str) -> CmdResult<()> {
        if dir == self.project_dir_name {
            return Ok(());
        }
        let next = match self.others.remove(dir) {
            Some(p) => p,
            None => open_dir(&self.home, &self.machine, dir)?,
        };
        let prev = std::mem::replace(&mut self.project, next);
        let prev_dir = std::mem::replace(&mut self.project_dir_name, dir.to_string());
        self.others.insert(prev_dir, prev);
        // It may have been edited on the other machine while it sat in the background.
        self.project.refresh().map_err(err)?;
        self.load_functions()?;
        Ok(())
    }

    /// Open the projects of all tabs (dropping tabs whose project can't be opened).
    fn load_tab_projects(&mut self) {
        let dirs: Vec<String> = self.tabs.iter().map(|t| t.project.clone()).collect();
        for d in dirs {
            if self.project_ref(&d).is_none() {
                match open_dir(&self.home, &self.machine, &d) {
                    Ok(p) => {
                        self.others.insert(d, p);
                    }
                    Err(_) => self.tabs.retain(|t| t.project != d),
                }
            }
        }
    }

    /// Add a tab after the current one, unless it is already open. Past the tab
    /// limit, the least recently shown unpinned tab closes (it stays on Home).
    fn add_tab(&mut self, tab: Tab) {
        if self.tabs.contains(&tab) {
            return;
        }
        let at = self.current.as_ref().and_then(|c| self.tabs.iter().position(|t| t == c)).map_or(self.tabs.len(), |i| i + 1);
        self.tabs.insert(at, tab.clone());
        while self.tabs.iter().filter(|t| !self.pinned.contains(t)).count() > self.max_tabs {
            let oldest = self
                .tabs
                .iter()
                .filter(|t| **t != tab && Some(*t) != self.current.as_ref() && !self.pinned.contains(t))
                .min_by_key(|t| self.opened.get(&tab_key(t)).copied().unwrap_or(0))
                .cloned();
            let Some(old) = oldest else { break };
            let name = self.project_ref(&old.project).and_then(|p| p.sheet(&old.sheet_id).ok()).map(|s| s.name).unwrap_or_default();
            self.tabs.retain(|t| *t != old);
            self.pending.push(format!("Closed “{name}”, the tab used least recently (at most {} open). It's on Home.", self.max_tabs));
        }
    }

    fn save_tabs(&self) {
        let f = TabsFile {
            tabs: self.tabs.clone(),
            current: self.current.clone(),
            pinned: self.pinned.clone(),
            max_tabs: (self.max_tabs != DEFAULT_MAX_TABS).then_some(self.max_tabs),
        };
        if let Ok(bytes) = serde_json::to_vec_pretty(&f) {
            let _ = fs::write(self.home.join(TABS_FILE), bytes);
        }
    }

    /// Remember that a sheet is being shown now (for Recent and the tab limit).
    fn mark_opened(&mut self, tab: &Tab) {
        self.opened.insert(tab_key(tab), chrono::Utc::now().timestamp_millis());
        if let Ok(bytes) = serde_json::to_vec(&self.opened) {
            let _ = fs::write(self.home.join(OPENED_FILE), bytes);
        }
    }

    /// Open every project (Unfiled included) so Home can list all sheets.
    fn load_all_projects(&mut self) {
        let mut dirs: Vec<String> = list_projects(&self.home).into_iter().map(|p| p.dir_name).collect();
        dirs.push(UNFILED.into());
        for d in dirs {
            if self.project_ref(&d).is_none() {
                if let Ok(p) = open_dir(&self.home, &self.machine, &d) {
                    self.others.insert(d, p);
                }
            }
        }
    }

    /// "Pasted", "Pasted 2", …: the first name not taken in the active project.
    fn unique_name(&self, base: &str, numbered: bool) -> CmdResult<String> {
        let taken: Vec<String> = self.project.sheets(true).map_err(err)?.into_iter().map(|s| s.name).collect();
        let first = if numbered { format!("{base} 1") } else { base.to_string() };
        if !taken.contains(&first) {
            return Ok(first);
        }
        Ok((2..).map(|n| format!("{base} {n}")).find(|n| !taken.contains(n)).unwrap_or(first))
    }

    /// Pick the sheet to show, keep the tabs in step, and build the snapshot.
    ///
    /// Shows `want` if it is a visible sheet of the active project (opening a tab for
    /// it), else the current tab, else a neighbour of a tab that went away, else
    /// nothing. Tabs whose sheet was archived or deleted (here or on the other
    /// machine) are dropped.
    fn respond(&mut self, want: Option<&str>, notices: Vec<String>) -> CmdResult<Snapshot> {
        let index_before = self.current.as_ref().and_then(|c| self.tabs.iter().position(|t| t == c)).unwrap_or(0);
        self.load_tab_projects();
        let tabs = std::mem::take(&mut self.tabs);
        self.tabs = tabs.into_iter().filter(|t| self.project_ref(&t.project).is_some_and(|p| visible(p, &t.sheet_id))).collect();

        let active = self.project_dir_name.clone();
        let mut chosen: Option<Tab> = want
            .filter(|id| visible(&self.project, id))
            .map(|id| Tab { project: active.clone(), sheet_id: id.to_string() })
            .or_else(|| self.current.clone().filter(|c| self.tabs.contains(c)));
        if chosen.is_none() && !self.tabs.is_empty() {
            chosen = Some(self.tabs[index_before.min(self.tabs.len() - 1)].clone());
        }
        if let Some(t) = &chosen {
            self.activate(&t.project)?;
            self.add_tab(t.clone());
            if self.current.as_ref() != Some(t) {
                self.mark_opened(t);
            }
        }
        self.current = chosen.clone();
        self.pinned.retain(|p| self.tabs.contains(p));
        self.save_tabs();
        let mut notices = notices;
        notices.append(&mut self.pending);
        self.snapshot(chosen.as_ref().map(|t| t.sheet_id.as_str()), notices)
    }

    fn new_sheet(&mut self, template: String, project: Option<String>) -> CmdResult<Snapshot> {
        let target = project.unwrap_or_else(|| UNFILED.to_string());
        self.activate(&target)?;
        let name = match template.as_str() {
            "spending" => self.unique_name("Spending", false)?,
            _ => self.unique_name("Sheet", true)?,
        };
        let id = Op::new_id();
        self.project.apply(Op::Batch { ops: templates::sheet_ops(&template, &id, &name) }).map_err(err)?;
        self.respond(Some(&id), vec![])
    }

    fn move_sheet(&mut self, project: String, sheet_id: String, to: Option<String>, new_project: Option<String>) -> CmdResult<Snapshot> {
        let to = match (to, new_project) {
            (_, Some(name)) if !name.trim().is_empty() => {
                let (p, dir) = create_project_in(&self.home, name.trim(), &self.machine, false)?;
                self.others.insert(dir.clone(), p);
                dir
            }
            (Some(to), _) => to,
            _ => return Err("Choose a project to move the sheet to.".into()),
        };
        if to == project {
            return self.respond(Some(&sheet_id), vec![]);
        }
        self.activate(&project)?;
        let from_settings = self.project.settings().clone();
        let name = self.project.sheet(&sheet_id).map_err(err)?.name;
        let new_id = Op::new_id();
        let ops = self.project.copy_sheet_ops(&sheet_id, &new_id).map_err(err)?;
        // Copy first: if that fails, the original is untouched.
        self.activate(&to)?;
        self.project.apply(Op::Batch { ops }).map_err(err)?;
        let to_settings = self.project.settings().clone();
        let to_name = to_settings.name.clone();
        // The original is archived under a name that says where it went, so it isn't
        // restored by mistake from Home's archived list.
        let left = format!("{name} (moved to {})", if to == UNFILED { "Unfiled" } else { to_name.as_str() });
        match self.others.get_mut(&project) {
            Some(src) => src
                .apply(Op::Batch {
                    ops: vec![
                        Op::RenameSheet { sheet_id: sheet_id.clone(), name: left },
                        Op::SetSheetArchived { sheet_id: sheet_id.clone(), archived: true },
                    ],
                })
                .map_err(err)?,
            None => return Err(format!("{project} is not open")),
        }
        // The tab now shows the copy, in the same place.
        let old = Tab { project, sheet_id };
        let new = Tab { project: to.clone(), sheet_id: new_id.clone() };
        match self.tabs.iter().position(|t| *t == old) {
            Some(i) => self.tabs[i] = new.clone(),
            None => self.add_tab(new.clone()),
        }
        self.current = Some(new);
        let changes = settings_changes(&from_settings, &to_settings);
        let place = if to == UNFILED { "Unfiled".to_string() } else { to_name.clone() };
        let mut notice = format!("Moved “{name}” to {place}.");
        if !changes.is_empty() {
            notice.push_str(&format!(" It now follows {place}'s settings: {}.", changes.join(", ")));
        }
        self.respond(Some(&new_id), vec![notice])
    }

    fn close_tab(&mut self, project: String, sheet_id: String, current: Option<String>) -> CmdResult<Snapshot> {
        let tab = Tab { project, sheet_id };
        let at = self.tabs.iter().position(|t| *t == tab);
        self.tabs.retain(|t| *t != tab);
        let want = if self.current.as_ref() == Some(&tab) {
            // The neighbour on the right, else on the left.
            let next = at.and_then(|i| self.tabs.get(i).or_else(|| i.checked_sub(1).and_then(|j| self.tabs.get(j)))).cloned();
            self.current = None;
            match next {
                Some(t) => {
                    self.activate(&t.project)?;
                    Some(t.sheet_id)
                }
                None => None,
            }
        } else {
            // Never show (and so reopen) the tab being closed.
            current.filter(|c| !(*c == tab.sheet_id && tab.project == self.project_dir_name))
        };
        self.respond(want.as_deref(), vec![])
    }

    fn open_project(&mut self, dir_name: String) -> CmdResult<Snapshot> {
        self.activate(&dir_name)?;
        let ids = visible_ids(&self.project);
        let mut notices = vec![];
        if ids.is_empty() {
            notices.push(format!("{} has no sheets yet.", self.project.settings().name));
        }
        // Open them in the project's order, after the current tab.
        for id in ids.iter().rev() {
            self.add_tab(Tab { project: dir_name.clone(), sheet_id: id.clone() });
        }
        self.respond(ids.first().map(|s| s.as_str()), notices)
    }

    fn new_sheet_from_text(&mut self, text: &str) -> CmdResult<Snapshot> {
        let table = tally_core::import::read_csv(text).map_err(err)?;
        self.activate(UNFILED)?;
        let name = self.unique_name("Pasted", false)?;
        let id = Op::new_id();
        self.project.apply(Op::Batch { ops: tally_core::import::sheet_ops(&table, &id, &name) }).map_err(err)?;
        let n = table.rows.len();
        self.respond(Some(&id), vec![format!("Made “{name}” from the clipboard: {n} rows. Double-click a header to rename or retype a column.")])
    }

    /// Every sheet in every project, for Home and Go to Sheet.
    fn sheet_index(&mut self, include_archived: bool) -> Vec<SheetEntry> {
        self.load_all_projects();
        let mut dirs: Vec<String> = self.others.keys().cloned().collect();
        dirs.push(self.project_dir_name.clone());
        let mut out = Vec::new();
        for d in dirs {
            let Some(p) = self.project_ref(&d) else { continue };
            let Ok(sheets) = p.sheets(include_archived) else { continue };
            for s in sheets {
                let tab = Tab { project: d.clone(), sheet_id: s.sheet_id.clone() };
                let columns = p.columns(&s.sheet_id).unwrap_or_default();
                use tally_core::ops::ColumnKind as K;
                let has = |f: &dyn Fn(&K) -> bool| columns.iter().any(|c| f(&c.spec.kind));
                let kind = if has(&|k| matches!(k, K::Money { .. }) || matches!(k, K::Calc { currency: Some(_), .. })) {
                    "money"
                } else if has(&|k| matches!(k, K::Date | K::DateTime)) {
                    "log"
                } else {
                    "table"
                };
                // The first totalled column's grand total (archived sheets are not worked out).
                let total = if s.archived {
                    None
                } else {
                    p.sheet_view(&s.sheet_id, &self.rates, today()).ok().and_then(|v| v.totals.into_iter().flatten().next())
                };
                out.push(SheetEntry {
                    project: d.clone(),
                    project_name: p.settings().name.clone(),
                    unfiled: d == UNFILED,
                    rows: p.rows(&s.sheet_id).map(|r| r.len()).unwrap_or(0),
                    columns: columns.len(),
                    kind: kind.into(),
                    total_currency: total.as_ref().and_then(|t| t.currency.clone()),
                    total: total.map(|t| t.amount),
                    edited: p.edited(&s.sheet_id).map(|t| t.timestamp_millis()),
                    opened: self.opened.get(&tab_key(&tab)).copied(),
                    open: self.tabs.contains(&tab),
                    pinned: self.pinned.contains(&tab),
                    sheet_id: s.sheet_id,
                    name: s.name,
                    archived: s.archived,
                });
            }
        }
        out
    }
}

/// What changes for a sheet moved from one project to another, for the notice.
fn settings_changes(from: &ProjectSettings, to: &ProjectSettings) -> Vec<String> {
    let mut out = Vec::new();
    if from.reporting_currency != to.reporting_currency {
        out.push(format!("reporting currency {} (was {})", to.reporting_currency, from.reporting_currency));
    }
    if from.tax_home != to.tax_home {
        let show = |t: &Option<String>| t.clone().unwrap_or_else(|| "none".into());
        out.push(format!("tax home {} (was {})", show(&to.tax_home), show(&from.tax_home)));
    }
    if from.rounding != to.rounding || from.rounding_scope != to.rounding_scope {
        out.push("rounding".into());
    }
    if from.rate_sources != to.rate_sources {
        out.push("default rate sources".into());
    }
    out
}

// ------------------------------------------------------------------ commands

#[tauri::command]
fn snapshot(state: State<AppState>, sheet_id: Option<String>) -> CmdResult<Snapshot> {
    guard(|| {
        let mut inner = state.lock();
        inner.respond(sheet_id.as_deref(), vec![])
    })
}

#[tauri::command]
fn apply(state: State<AppState>, op: Op, sheet_id: Option<String>) -> CmdResult<Snapshot> {
    guard(|| {
        let mut inner = state.lock();
        inner.project.apply(op).map_err(err)?;
        inner.respond(sheet_id.as_deref(), vec![])
    })
}

#[tauri::command]
fn undo(state: State<AppState>, sheet_id: Option<String>) -> CmdResult<Snapshot> {
    guard(|| {
        let mut inner = state.lock();
        let before = visible_ids(&inner.project);
        inner.project.undo().map_err(err)?;
        let back = revived(&before, &inner.project);
        inner.respond(back.as_deref().or(sheet_id.as_deref()), vec![])
    })
}

/// A sheet that undo or redo brought back (an archived or moved one), to show it.
fn revived(before: &[String], p: &Project) -> Option<String> {
    visible_ids(p).into_iter().find(|id| !before.contains(id))
}

#[tauri::command]
fn redo(state: State<AppState>, sheet_id: Option<String>) -> CmdResult<Snapshot> {
    guard(|| {
        let mut inner = state.lock();
        let before = visible_ids(&inner.project);
        inner.project.redo().map_err(err)?;
        let back = revived(&before, &inner.project);
        inner.respond(back.as_deref().or(sheet_id.as_deref()), vec![])
    })
}

/// Pick up changes synced from other machines (project log and rate library), and
/// sheets imported from the command line.
#[tauri::command]
fn sync(state: State<AppState>, sheet_id: Option<String>) -> CmdResult<Option<Snapshot>> {
    guard(|| {
        let mut inner = state.lock();
        let before = inner.rates.len();
        inner.reload_rates()?;
        let before_fns = inner.project.functions().list().len();
        inner.load_functions()?;
        let fns_changed = inner.project.functions().list().len() != before_fns;
        let changed = inner.project.refresh().map_err(err)? || inner.rates.len() != before || fns_changed;
        // Files imported with `tally import`: open and show them.
        if let Some(t) = inner.open_requested() {
            inner.activate(&t.project)?;
            let mut s = inner.respond(Some(&t.sheet_id), vec![])?;
            s.focus = true;
            return Ok(Some(s));
        }
        if changed {
            Ok(Some(inner.respond(sheet_id.as_deref(), vec![])?))
        } else {
            Ok(None)
        }
    })
}

/// Open all of a project's sheets as tabs and show the first.
#[tauri::command]
fn open_project(state: State<AppState>, dir_name: String) -> CmdResult<Snapshot> {
    guard(|| {
        let mut inner = state.lock();
        inner.open_project(dir_name)
    })
}

/// A new project with a spending list, opened as a tab.
#[tauri::command]
fn create_project(state: State<AppState>, name: String) -> CmdResult<Snapshot> {
    guard(|| {
        let mut inner = state.lock();
        let (project, dir_name) = create_project_in(&inner.home, &name, &inner.machine, true)?;
        let first = visible_ids(&project).into_iter().next();
        inner.others.insert(dir_name.clone(), project);
        inner.activate(&dir_name)?;
        inner.respond(first.as_deref(), vec![])
    })
}

/// Show a sheet of any project, opening a tab for it if needed.
#[tauri::command]
fn open_sheet(state: State<AppState>, project: String, sheet_id: String) -> CmdResult<Snapshot> {
    guard(|| {
        let mut inner = state.lock();
        inner.activate(&project)?;
        inner.respond(Some(&sheet_id), vec![])
    })
}

/// Close a tab. Nothing is archived or deleted; the sheet stays in its project.
/// Closing the shown tab shows its neighbour.
#[tauri::command]
fn close_tab(state: State<AppState>, project: String, sheet_id: String, current: Option<String>) -> CmdResult<Snapshot> {
    guard(|| {
        let mut inner = state.lock();
        inner.close_tab(project, sheet_id, current)
    })
}

#[tauri::command]
fn save_settings(state: State<AppState>, settings: ProjectSettings, sheet_id: Option<String>) -> CmdResult<Snapshot> {
    guard(|| {
        let mut inner = state.lock();
        inner.project.save_settings(settings).map_err(err)?;
        inner.respond(sheet_id.as_deref(), vec![])
    })
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

    guard(|| {
        let mut inner = state.lock();
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
    })
}

/// Import a rates file downloaded by hand (ECB XML, NBU JSON or HMRC CSV).
#[tauri::command]
fn import_rates(state: State<AppState>, source: RateSource, path: String) -> CmdResult<usize> {
    guard(|| {
        let text = fs::read_to_string(&path).map_err(err)?;
        let records = sources::parse(source, &text).map_err(err)?;
        let mut inner = state.lock();
        let added = inner.library.add_rates(&records).map_err(err)?;
        inner.reload_rates()?;
        Ok(added)
    })
}

/// Add a rate by hand: `1 base = rate quote` from `date`.
#[tauri::command]
fn add_manual_rate(state: State<AppState>, base: String, quote: String, rate: String, date: NaiveDate) -> CmdResult<usize> {
    guard(|| {
        let (rate, _) = tally_core::money::parse_amount(&rate).map_err(err)?;
        let rec = RateRecord {
            date,
            valid_to: None,
            base: Currency::new(&base).map_err(err)?,
            quote: Currency::new(&quote).map_err(err)?,
            rate,
            source: RateSource::Manual,
        };
        let mut inner = state.lock();
        let added = inner.library.add_rates(&[rec]).map_err(err)?;
        inner.reload_rates()?;
        Ok(added)
    })
}

/// A new sheet from a template ("blank" or "spending"), in Unfiled unless a
/// project is given, opened in a tab after the current one. Asks nothing.
#[tauri::command]
fn new_sheet(state: State<AppState>, template: String, project: Option<String>) -> CmdResult<Snapshot> {
    guard(|| {
        let mut inner = state.lock();
        inner.new_sheet(template, project)
    })
}

/// File a sheet in another project (or a new one, or Unfiled). The copy is made in
/// the target as one undoable step and the original is archived in its project, so
/// nothing is lost. Its tab stays where it was, now showing the copy.
#[tauri::command]
fn move_sheet(state: State<AppState>, project: String, sheet_id: String, to: Option<String>, new_project: Option<String>) -> CmdResult<Snapshot> {
    guard(|| {
        let mut inner = state.lock();
        inner.move_sheet(project, sheet_id, to, new_project)
    })
}

fn current_view(state: &State<AppState>, sheet_id: &str) -> CmdResult<tally_core::sheet::SheetView> {
    let inner = state.lock();
    inner.project.sheet_view(sheet_id, &inner.rates, today()).map_err(err)
}

/// Columns over time (bucketed to about `max_points`).
#[tauri::command]
fn analyze_series(state: State<AppState>, sheet_id: String, x: Option<String>, cols: Vec<String>, max_points: usize) -> CmdResult<analysis::Series> {
    guard(|| {
        let view = current_view(&state, &sheet_id)?;
        analysis::series(&view, x.as_deref(), &cols, max_points.max(10)).map_err(err)
    })
}

/// A column's distribution: quartiles, histogram, normal fit (or category counts).
#[tauri::command]
fn analyze_distribution(state: State<AppState>, sheet_id: String, col: String) -> CmdResult<analysis::Distribution> {
    guard(|| {
        let view = current_view(&state, &sheet_id)?;
        analysis::distribution(&view, &col).map_err(err)
    })
}

/// Rows grouped by a column (or time buckets), with aggregates of `cols`.
#[tauri::command]
fn analyze_groups(state: State<AppState>, sheet_id: String, by: String, cols: Vec<String>, bucket: Option<f64>) -> CmdResult<analysis::Groups> {
    guard(|| {
        let view = current_view(&state, &sheet_id)?;
        analysis::groups(&view, &by, &cols, bucket).map_err(err)
    })
}

/// Keep a group summary as a new sheet.
#[tauri::command]
fn save_groups_sheet(
    state: State<AppState>,
    sheet_id: String,
    by: String,
    cols: Vec<String>,
    bucket: Option<f64>,
    name: String,
) -> CmdResult<Snapshot> {
    guard(|| {
        let view = current_view(&state, &sheet_id)?;
        let g = analysis::groups(&view, &by, &cols, bucket).map_err(err)?;
        let table = analysis::groups_table(&g, |secs| {
            chrono::DateTime::from_timestamp_micros((secs * 1e6) as i64).map_or(secs.to_string(), |t| tally_core::time::show_moment(&t))
        });
        let mut inner = state.lock();
        let id = Op::new_id();
        inner.project.apply(Op::Batch { ops: tally_core::import::sheet_ops(&table, &id, &name) }).map_err(err)?;
        inner.respond(Some(&id), vec![])
    })
}

/// Write bytes (a chart image) to a file the user chose.
#[tauri::command]
fn write_file(path: String, bytes: Vec<u8>) -> CmdResult<()> {
    guard(|| {
        std::fs::write(path, bytes).map_err(err)
    })
}

/// Every sheet in every project, for Home and Go to Sheet.
#[tauri::command]
fn sheet_index(state: State<AppState>, include_archived: bool) -> CmdResult<Vec<SheetEntry>> {
    guard(|| {
        let mut inner = state.lock();
        Ok(inner.sheet_index(include_archived))
    })
}

/// Bring back an archived sheet and show it.
#[tauri::command]
fn restore_sheet(state: State<AppState>, project: String, sheet_id: String) -> CmdResult<Snapshot> {
    guard(|| {
        let mut inner = state.lock();
        inner.activate(&project)?;
        inner.project.apply(Op::SetSheetArchived { sheet_id: sheet_id.clone(), archived: false }).map_err(err)?;
        inner.respond(Some(&sheet_id), vec![])
    })
}

/// Pin or unpin a tab: pinned tabs are never closed to make room for new ones.
#[tauri::command]
fn pin_tab(state: State<AppState>, project: String, sheet_id: String, pinned: bool, current: Option<String>) -> CmdResult<Snapshot> {
    guard(|| {
        let mut inner = state.lock();
        let tab = Tab { project, sheet_id };
        inner.pinned.retain(|t| *t != tab);
        if pinned {
            inner.pinned.push(tab);
        }
        inner.respond(current.as_deref(), vec![])
    })
}

/// A new Unfiled sheet from text copied from Excel, Numbers or Google Sheets (tab
/// separated) or CSV: the first row becomes the headings, columns typed from the values.
#[tauri::command]
fn new_sheet_from_text(state: State<AppState>, text: String) -> CmdResult<Snapshot> {
    guard(|| {
        if text.trim().is_empty() {
            return Err("The clipboard is empty. Copy some cells first.".into());
        }
        let mut inner = state.lock();
        inner.new_sheet_from_text(&text)
    })
}

/// Add a file as new sheets (one for CSV or JSON, one per worksheet of an Excel
/// workbook) in one undoable step; columns are typed from the values. Shows the first.
#[tauri::command]
fn import_file(state: State<AppState>, path: String) -> CmdResult<Snapshot> {
    guard(|| {
        let tables = tally_core::import::read_tables(Path::new(&path)).map_err(err)?;
        let mut inner = state.lock();
        // Imports land in Unfiled, like every new sheet; file them with Move to project.
        inner.activate(UNFILED)?;
        let mut ops = Vec::new();
        let mut ids = Vec::new();
        for (name, table) in &tables {
            let id = Op::new_id();
            ops.extend(tally_core::import::sheet_ops(table, &id, name));
            ids.push(id);
        }
        inner.project.apply(Op::Batch { ops }).map_err(err)?;
        for id in ids.iter().skip(1).rev() {
            inner.add_tab(Tab { project: UNFILED.into(), sheet_id: id.clone() });
        }
        inner.respond(ids.first().map(|s| s.as_str()), vec![])
    })
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
    guard(|| {
        let inner = state.lock();
        let currency = currency.map(|c| Currency::new(&c)).transpose().map_err(err)?;
        inner.project.preview_formula(&sheet_id, col_id.as_deref(), &formula, currency, &inner.rates, today()).map_err(err)
    })
}

/// Save a user function (shared by every sheet and project); `old_name` renames one.
#[tauri::command]
fn save_function(state: State<AppState>, function: UserFunction, old_name: Option<String>, sheet_id: Option<String>) -> CmdResult<Snapshot> {
    guard(|| {
        let mut inner = state.lock();
        inner.library.save_function(function, old_name.as_deref()).map_err(err)?;
        let problems = inner.load_functions()?;
        inner.respond(sheet_id.as_deref(), problems)
    })
}

#[tauri::command]
fn delete_function(state: State<AppState>, name: String, sheet_id: Option<String>) -> CmdResult<Snapshot> {
    guard(|| {
        let mut inner = state.lock();
        inner.library.delete_function(&name).map_err(err)?;
        let problems = inner.load_functions()?;
        inner.respond(sheet_id.as_deref(), problems)
    })
}

/// Try an expression such as `vat(100)` with a draft of a function in place.
#[tauri::command]
fn try_function(state: State<AppState>, function: UserFunction, expression: String) -> CmdResult<String> {
    guard(|| {
        let inner = state.lock();
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
            Value::Date(d) => tally_core::time::store_date(&d),
            Value::Time(t) => tally_core::time::show_moment(&t),
            Value::Error(e) => return Err(e),
        })
    })
}

#[tauri::command]
fn export_xlsx(state: State<AppState>, sheet_id: String, path: String) -> CmdResult<()> {
    guard(|| {
        let inner = state.lock();
        let view = inner.project.sheet_view(&sheet_id, &inner.rates, today()).map_err(err)?;
        tally_core::export::export_xlsx(&view, Path::new(&path)).map_err(err)
    })
}

/// Export a sheet as xlsx, pdf, csv or json (chosen by `format`).
#[tauri::command]
fn export_sheet(state: State<AppState>, sheet_id: String, path: String, format: String) -> CmdResult<()> {
    guard(|| {
        let inner = state.lock();
        let view = inner.project.sheet_view(&sheet_id, &inner.rates, today()).map_err(err)?;
        let project = inner.project.settings().name.clone();
        drop(inner);
        let p = Path::new(&path);
        match format.as_str() {
            "xlsx" => tally_core::export::export_xlsx(&view, p).map_err(err),
            "csv" => std::fs::write(p, tally_core::formats::to_csv(&view).map_err(err)?).map_err(err),
            "json" => std::fs::write(p, serde_json::to_string_pretty(&tally_core::formats::to_json(&view, &project)).map_err(err)?).map_err(err),
            "pdf" => std::fs::write(p, tally_core::formats::to_pdf(&view, &project).map_err(err)?).map_err(err),
            other => Err(format!("unknown export format {other}")),
        }
    })
}

/// The macOS app menu. Tauri's default menu binds ⌘W to Close Window, and closing the
/// only window quits Tally; here ⌘W is Close Tab (the UI decides what that closes) and
/// Close Window moves to ⇧⌘W, as in Safari. The Edit items stay so ⌘C/⌘V/⌘Z keep
/// working in text fields. Linux gets no menu bar; the UI handles Ctrl+W itself.
#[cfg(target_os = "macos")]
fn app_menu<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> tauri::Result<tauri::menu::Menu<R>> {
    use tauri::menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder};
    let name = app.package_info().name.clone();
    let tally = SubmenuBuilder::new(app, &name)
        .about(None)
        .separator()
        .services()
        .separator()
        .hide()
        .hide_others()
        .show_all()
        .separator()
        .quit()
        .build()?;
    let close_tab = MenuItemBuilder::with_id(MENU_CLOSE_TAB, "Close Tab").accelerator("CmdOrCtrl+W").build(app)?;
    let close_window = MenuItemBuilder::with_id(MENU_CLOSE_WINDOW, "Close Window")
        .accelerator("CmdOrCtrl+Shift+W")
        .build(app)?;
    let file = SubmenuBuilder::new(app, "File").item(&close_tab).item(&close_window).build()?;
    let edit = SubmenuBuilder::new(app, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;
    let view = SubmenuBuilder::new(app, "View").fullscreen().build()?;
    let window = SubmenuBuilder::new(app, "Window").minimize().maximize().build()?;
    MenuBuilder::new(app).items(&[&tally, &file, &edit, &view, &window]).build()
}

/// Menu item IDs. Close Tab is sent to the UI as the `close-tab` event.
const MENU_CLOSE_TAB: &str = "close_tab";
const MENU_CLOSE_WINDOW: &str = "close_window";

fn on_menu<R: tauri::Runtime>(app: &tauri::AppHandle<R>, event: tauri::menu::MenuEvent) {
    use tauri::{Emitter, Manager};
    match event.id().as_ref() {
        MENU_CLOSE_TAB => {
            let _ = app.emit("close-tab", ());
        }
        MENU_CLOSE_WINDOW => {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.close();
            }
        }
        _ => {}
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let inner = init().expect("could not open the Tally folder");
    install_panic_hook(inner.home.clone());
    let builder = tauri::Builder::default();
    #[cfg(target_os = "macos")]
    let builder = builder.menu(|app| app_menu(app));
    builder
        .on_menu_event(on_menu)
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
            new_sheet,
            open_sheet,
            close_tab,
            move_sheet,
            sheet_index,
            restore_sheet,
            pin_tab,
            new_sheet_from_text,
            preview_formula,
            save_function,
            delete_function,
            try_function,
            export_xlsx,
            export_sheet,
            import_file,
            analyze_series,
            analyze_distribution,
            analyze_groups,
            save_groups_sheet,
            write_file
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tally");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> (tempfile::TempDir, Inner) {
        let tmp = tempfile::tempdir().unwrap();
        let inner = init_at(tmp.path().to_path_buf(), "mac".into()).unwrap();
        (tmp, inner)
    }

    #[test]
    fn a_panicking_command_fails_with_an_error_and_the_app_carries_on() {
        let (tmp, inner) = fresh();
        install_panic_hook(tmp.path().to_path_buf());
        let state = AppState(Mutex::new(inner));
        let result: CmdResult<()> = guard(|| {
            let _held = state.lock();
            let text = String::from("Кафе");
            let _ = &text[..std::hint::black_box(3)]; // inside 'а': panics
            Ok(())
        });
        let message = result.unwrap_err();
        assert!(message.contains("not a char boundary") && message.contains("lib.rs"), "{message}");
        // The panic poisoned the lock while it was held; the next command still gets the state.
        assert!(state.0.is_poisoned());
        assert!(state.lock().respond(None, vec![]).is_ok());
        assert!(!state.0.is_poisoned());
        let log = fs::read_to_string(tmp.path().join(PANIC_LOG)).unwrap();
        assert!(log.contains("not a char boundary"), "{log}");
    }

    #[test]
    fn projects_get_distinct_colours_and_keep_them() {
        let (tmp, _inner) = fresh();
        let home = tmp.path();
        for n in ["Flat", "Tax", "Car", "Garden"] {
            create_project_in(home, n, "mac", false).unwrap();
        }
        let hues = |h: &Path| list_projects(h).into_iter().map(|p| (p.name, p.hue.unwrap())).collect::<Vec<_>>();
        let first = hues(home);
        let mut distinct: Vec<u16> = first.iter().map(|(_, h)| *h).collect();
        distinct.sort();
        distinct.dedup();
        assert_eq!(distinct.len(), 4, "{first:?}");
        // A project from before colours gets one at start-up; the others keep theirs.
        let dir = projects_dir(home).join("old");
        Project::create(&dir, default_settings("Old"), "mac").unwrap();
        assert_eq!(read_settings(&dir).unwrap().color, None);
        let _again = init_at(home.to_path_buf(), "mac".into()).unwrap();
        let after = hues(home);
        assert!(first.iter().all(|x| after.contains(x)), "{after:?}");
        let old = after.iter().find(|(n, _)| n == "Old").unwrap().1;
        assert!(!distinct.contains(&old), "{after:?}");
        // Unfiled has no colour.
        assert_eq!(read_settings(&projects_dir(home).join(UNFILED)).unwrap().color, None);
    }

    fn names(s: &Snapshot) -> Vec<(String, String)> {
        s.tabs.iter().map(|t| (t.name.clone(), if t.unfiled { "Unfiled".into() } else { t.project_name.clone() })).collect()
    }

    fn shown(s: &Snapshot) -> Option<String> {
        s.sheet.as_ref().map(|v| v.name.clone())
    }

    #[test]
    fn first_run_starts_with_a_spending_list_in_unfiled() {
        let (_tmp, mut inner) = fresh();
        let s = inner.respond(None, vec![]).unwrap();
        assert_eq!(names(&s), [("Spending".to_string(), "Unfiled".to_string())]);
        assert_eq!(shown(&s).as_deref(), Some("Spending"));
        assert!(s.projects.is_empty(), "Unfiled is not listed among projects");
    }

    #[test]
    fn new_sheets_go_to_unfiled_and_open_after_the_current_tab() {
        let (_tmp, mut inner) = fresh();
        let (p, dir) = create_project_in(&inner.home, "Drone build", &inner.machine, true).unwrap();
        inner.others.insert(dir.clone(), p);
        inner.open_project(dir.clone()).unwrap();
        // Shown: Drone build's Spending. A new sheet asks nothing and lands in Unfiled.
        let s = inner.new_sheet("blank".into(), None).unwrap();
        assert_eq!(shown(&s).as_deref(), Some("Sheet 1"));
        assert_eq!(s.project, UNFILED);
        assert_eq!(
            names(&s),
            [("Spending".into(), "Unfiled".into()), ("Spending".into(), "Drone build".into()), ("Sheet 1".into(), "Unfiled".into())]
        );
        // Started inside a project, it goes there.
        let s = inner.new_sheet("blank".into(), Some(dir)).unwrap();
        assert_eq!(s.tabs.last().map(|t| t.project_name.as_str()), Some("Drone build"));
        assert_eq!(shown(&s).as_deref(), Some("Sheet 1"));
    }

    #[test]
    fn moving_a_sheet_keeps_its_tab_and_archives_the_original() {
        let (_tmp, mut inner) = fresh();
        let (p, dir) = create_project_in(&inner.home, "UK tax", &inner.machine, false).unwrap();
        inner.others.insert(dir.clone(), p);
        let s = inner.respond(None, vec![]).unwrap();
        let id = s.tabs[0].sheet_id.clone();

        let s = inner.move_sheet(UNFILED.into(), id.clone(), Some(dir.clone()), None).unwrap();
        assert_eq!(names(&s), [("Spending".to_string(), "UK tax".to_string())]);
        assert_eq!(shown(&s).as_deref(), Some("Spending"));
        assert_eq!(s.project, dir);
        assert!(s.notices[0].contains("reporting currency GBP (was USD)"), "{:?}", s.notices);
        let unfiled = inner.project_ref(UNFILED).unwrap();
        let original = unfiled.sheet(&id).unwrap();
        assert!(original.archived, "the original is archived, not deleted");
        assert_eq!(original.name, "Spending (moved to UK tax)");
        // The copy has the same columns.
        let copy = &s.tabs[0].sheet_id;
        assert_eq!(inner.project.columns(copy).unwrap().len(), unfiled.columns(&id).unwrap().len());

        // Undo in the target takes the copy away; its tab goes with it.
        let s = { inner.project.undo().unwrap(); inner.respond(None, vec![]).unwrap() };
        assert!(s.tabs.is_empty());
        assert!(s.sheet.is_none());
    }

    #[test]
    fn moving_to_a_new_project_creates_it_empty() {
        let (_tmp, mut inner) = fresh();
        let s = inner.respond(None, vec![]).unwrap();
        let id = s.tabs[0].sheet_id.clone();
        let s = inner.move_sheet(UNFILED.into(), id, None, Some("Tax 2026".into())).unwrap();
        assert_eq!(s.projects.len(), 1);
        assert_eq!(s.projects[0].name, "Tax 2026");
        assert_eq!(inner.project.sheets(false).unwrap().len(), 1, "only the moved sheet, no starter sheet");
    }

    #[test]
    fn closing_tabs_shows_a_neighbour_and_keeps_the_sheet() {
        let (_tmp, mut inner) = fresh();
        inner.new_sheet("blank".into(), None).unwrap();
        let s = inner.new_sheet("blank".into(), None).unwrap();
        assert_eq!(names(&s).len(), 3);
        // Close the shown tab (the last one): its left neighbour shows.
        let t = s.tabs[2].clone();
        let s = inner.close_tab(t.project, t.sheet_id.clone(), Some(t.sheet_id.clone())).unwrap();
        assert_eq!(shown(&s).as_deref(), Some("Sheet 1"));
        assert_eq!(s.tabs.len(), 2);
        assert!(visible(&inner.project, &t.sheet_id), "closing a tab never archives");
        for t in s.tabs.clone() {
            inner.close_tab(t.project, t.sheet_id.clone(), Some(t.sheet_id)).unwrap();
        }
        let s = inner.respond(None, vec![]).unwrap();
        assert!(s.tabs.is_empty() && s.sheet.is_none());
    }

    #[test]
    fn tabs_survive_a_restart() {
        let (tmp, mut inner) = fresh();
        inner.new_sheet("spending".into(), None).unwrap();
        let before = names(&inner.respond(None, vec![]).unwrap());
        drop(inner);
        let mut again = init_at(tmp.path().to_path_buf(), "mac".into()).unwrap();
        let s = again.respond(None, vec![]).unwrap();
        assert_eq!(names(&s), before);
        assert_eq!(shown(&s).as_deref(), Some("Spending 2"));
    }

    /// What `tally import` does: add a sheet to a project's log from outside the
    /// app (another `Project` on the same folder), then leave an open request.
    fn import_from_command_line(home: &Path, dir: &str, csv: &str, name: &str) -> String {
        let mut p = open_dir(home, "mac", dir).unwrap();
        let table = tally_core::import::read_csv(csv).unwrap();
        let id = Op::new_id();
        p.apply(Op::Batch { ops: tally_core::import::sheet_ops(&table, &id, name) }).unwrap();
        let req = tally_core::home::OpenRequest { project: dir.into(), sheet_ids: vec![id.clone()] };
        tally_core::home::request_open(home, "mac", &req).unwrap();
        id
    }

    #[test]
    fn sheets_imported_from_the_command_line_open_while_the_app_runs() {
        let (tmp, mut inner) = fresh();
        inner.respond(None, vec![]).unwrap(); // Unfiled is open and cached
        import_from_command_line(tmp.path(), UNFILED, "Item,USD\nKettle,25\n", "Kitchen");
        let t = inner.open_requested().expect("a tab to show");
        inner.activate(&t.project).unwrap();
        let s = inner.respond(Some(&t.sheet_id), vec![]).unwrap();
        assert_eq!(shown(&s).as_deref(), Some("Kitchen"));
        assert_eq!(s.sheet.as_ref().unwrap().rows.len(), 1);
        assert!(s.notices.iter().any(|n| n.contains("Opened “Kitchen” from the command line")));
        assert!(inner.open_requested().is_none(), "a request is taken once");
    }

    #[test]
    fn sheets_imported_while_the_app_was_closed_open_when_it_starts() {
        let (tmp, mut inner) = fresh();
        inner.respond(None, vec![]).unwrap();
        drop(inner);
        let (_, dir) = create_project_in(tmp.path(), "Flat", "mac", false).unwrap();
        import_from_command_line(tmp.path(), &dir, "Item,UAH\nTiles,1200\n", "Bathroom");
        let mut again = init_at(tmp.path().to_path_buf(), "mac".into()).unwrap();
        let s = again.respond(None, vec![]).unwrap();
        assert_eq!(shown(&s).as_deref(), Some("Bathroom"));
        assert!(names(&s).contains(&("Bathroom".into(), "Flat".into())));
    }

    #[test]
    fn past_the_limit_the_least_recently_used_unpinned_tab_closes() {
        let (_tmp, mut inner) = fresh();
        inner.max_tabs = 3;
        let first = inner.respond(None, vec![]).unwrap().tabs[0].clone();
        inner.pinned.push(Tab { project: first.project.clone(), sheet_id: first.sheet_id.clone() });
        let mut last = None;
        for _ in 0..4 {
            std::thread::sleep(std::time::Duration::from_millis(3));
            last = Some(inner.new_sheet("blank".into(), None).unwrap());
        }
        let s = last.unwrap();
        // The pinned tab plus three others.
        assert_eq!(s.tabs.len(), 4, "{:?}", names(&s));
        assert!(s.tabs[0].pinned);
        assert!(!s.tabs.iter().any(|t| t.name == "Sheet 1"), "the oldest unpinned tab closed: {:?}", names(&s));
        assert!(s.notices.iter().any(|n| n.contains("Closed “Sheet 1”")), "{:?}", s.notices);
    }

    #[test]
    fn the_index_lists_every_sheet_in_every_project() {
        let (_tmp, mut inner) = fresh();
        let (p, dir) = create_project_in(&inner.home, "Drone build", &inner.machine, true).unwrap();
        inner.others.insert(dir, p);
        let idx = inner.sheet_index(false);
        assert_eq!(idx.len(), 2);
        assert!(idx.iter().all(|e| e.kind == "money"));
        assert!(idx.iter().any(|e| e.unfiled && e.open));
        assert!(idx.iter().any(|e| e.project_name == "Drone build" && !e.open && e.edited.is_some()));
    }

    #[test]
    fn cells_copied_from_excel_become_an_unfiled_sheet() {
        let (_tmp, mut inner) = fresh();
        let s = inner.new_sheet_from_text("Item\tPrice (UAH)\tQty\nPlates\t3250\t2\nMotors\t1800\t4\n").unwrap();
        assert_eq!(shown(&s).as_deref(), Some("Pasted"));
        assert_eq!(s.project, UNFILED);
        let v = s.sheet.unwrap();
        assert_eq!(v.rows.len(), 2);
        assert_eq!(v.columns.len(), 3);
        let s = inner.new_sheet_from_text("a,b\n1,2\n").unwrap();
        assert_eq!(shown(&s).as_deref(), Some("Pasted 2"));
    }
}
