# Plan: sheets first, projects optional (Home, Unfiled, tabs from any project)

Decided 2026-11-06. Idea taken from unacro's file handling: organising things is a view over everything, never something you have to choose before you start.

## Problem

The project dropdown is a mode. Tally reopens the last project, and new sheets (+, Import) always go into the current project. Putting a sheet anywhere else means switching or creating a project first, so unrelated sheets pile up in whatever project happens to be open.

## Principles

- **Starting a sheet never asks where it goes.** New sheets go to **Unfiled** unless started from inside a project.
- **Filing comes later and is visible.** Every tab in a project shows the project's name after the sheet's; Move to project… on the tab's right-click menu refiles the sheet.
- **Tabs are a working set from any project.** Closing a tab never deletes or archives anything.
- **Projects are for shared settings** (tax home, reporting currency, rate sources, rounding, AI notes) and for summaries across sheets, not a place you must enter first.
- **"Home"** is the name of the tab that lists every sheet. "Library" stays the reference library (products, rates, tax rates, user functions).

## Phase 1: Unfiled, ⌘N, project chips, Move to project

- [x] **Core:** `Project::copy_sheet_ops(sheet_id, new_id)` produces the ops that recreate a sheet (settings, rate card, columns with their IDs, rows, section rows, cells) in another project. Tests.
- [x] **Unfiled:** a hidden project folder `projects/_unfiled` (settings: USD reporting, no tax home). Not listed among projects; its sheets show in tabs (and later on Home) like any other.
- [x] **Backend holds several projects:** the active project is the current tab's project; the others stay open in a cache, refreshed from sync when activated. Undo and redo act on the current tab's project.
- [x] **Open tabs** kept per machine in Tally's home folder (`.tabs.json`), replacing `.last-project`. The first run after the update opens the last project's sheets as tabs, so nothing disappears.
- [x] **⌘N** makes a new blank sheet in Unfiled and opens it. No questions.
- [x] **+ menu:** Blank, Spending list and From a file… all go to Unfiled; one extra entry, "New sheet in ‹project›", when the current tab belongs to a project.
- [x] **Import** (From a file…) creates its sheets in Unfiled.
- [x] **Tabs:** sheets from any project, each with a project chip. × closes the tab. Right-click: Rename, Move to project, Archive, Close. ⌘PageUp/PageDown cycle tabs.
- [x] **Move to project:** list of projects, Unfiled, and "New project…" (creates the project and moves the sheet into it). The copy goes into the target as one undoable step, and the original is archived in the source. If the two projects' settings differ (reporting currency, rounding, tax home), a notice says what changed.
- [x] **Open project** picker in the top bar (replacing the project dropdown) opens all of a project's sheets as tabs. "New project…" stays. Removed in phase 2 once Home exists.
- [x] **Check:** core tests, the app compiles, the UI type-checks, then a real run on the Mac.

Known limitation: a move is two steps in two projects, so ⌘Z after a move removes the copy from the target while the original stays archived in the source. The way to undo a move is to move the sheet back.

## Phase 2: Home and Go to Sheet

- [x] **Home tab**, first tab and always there: every sheet in every project, viewed by Recent, Timeline (last 5 minutes, last hour, today by hour, yesterday, this week, then months), Project, or Kind (spending, tax, imported, ledger). Search as you type. Each row shows the project, rows and total; archived sheets in a separate filter.
- [x] **New sheet from Home**, in Unfiled or in a chosen project group.
- [x] **Go to Sheet (⌘K)**: fuzzy search over sheet and project names, open tabs first. (⌘J stays the column finder.)
- [x] **Last edited** from op-log timestamps (synced); **last opened** per machine in Tally's home folder.
- [x] **Tab working set:** at most n tabs (12 by default); opening more closes the least recently used, never a pinned one. Pin from the tab's menu.
- [x] **New sheet from clipboard** (⇧⌘N): paste a block copied from Excel and it becomes a new Unfiled sheet.
- [x] Remove the Open project picker.

## Phase 3: docs

- [x] Update the goals doc's Core concepts and `docs/GOALS.md`: no current project; lookups search the sheet's own project first, then global; Unfiled and Home described.
- [x] README: keyboard table (⌘N, ⌘K, ⇧⌘N, tab menu).

## Status (2026-11-06)

All three phases are built. Checked: tally-core tests (including copying a sheet between projects), app tests for tabs, Unfiled, moving, closing, the tab limit and the index, `cargo check` of the app, and `tsc`. Still to do: a real run of the app on the Mac.

Calls made while building (change any of them freely):

- ⌘N makes a **blank** sheet ("Sheet 1", "Sheet 2", … per project). The + menu still offers a spending list.
- **Kind** on Home is worked out from the columns, since sheets have no type yet: Money (any money column, or a calculated column with a currency), Logs (dates or times), Tables (the rest). Spending, tax and ledger kinds can come with sheet types.
- The tab limit is 12 unpinned tabs. It is kept per machine in `.tabs.json` as `max_tabs`; there is no settings screen for it yet.
- New sheet from clipboard reads the first row as headings, like a CSV import, and names the sheet "Pasted", "Pasted 2", ….
- Home shows each sheet's first total (the first column marked "Show total"). Archived sheets are not worked out, only listed.
- "Last used" on Home is the later of last edited (synced, from the op logs) and last opened (this machine).
- Closing a tab while Home is shown stays on Home. Closing the last tab shows Home.
- Home's toolbar has New project…; a new project made there starts with a spending list, while one made from Move to project starts empty.

## Fixes after first use (2026-11-08)

- [x] **+ did nothing** when it sat at the right edge: its 300px menu opened at the button's left edge, almost entirely off-screen, and its invisible backdrop swallowed the next click. Tab menus now always open inside the window.
- [x] **+ always in reach:** only the sheet tabs between Home and + scroll (sideways, also with a mouse wheel; no scrollbar; faded edges show more tabs that way). The shown tab scrolls into view.
- [x] **⌘W closed the window and quit Tally.** On macOS Tally now has its own menu: File ▸ Close Tab (⌘W) and Close Window (⇧⌘W). ⌘W closes an open panel or finder first, otherwise the shown tab; on Home it does nothing. Linux has no menu bar, so the UI handles Ctrl+W.
- [x] **Project chips looked like tabs.** Replaced by the project's name in quiet text after the sheet name ("Kitchen · Flat renovation"); Unfiled sheets show nothing. Moving is on the tab's right-click menu.

## Open questions

- Should Unfiled sheets that haven't been touched for months be suggested for archiving on Home?
- Should the tab limit get a setting in the app?
