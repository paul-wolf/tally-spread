# Tally

A fast, AI-driven tool for everyday money work: cost lists in any currency, totals in several, exchange rates and taxes handled for you.

Tally is not an Excel clone. It is a keyboard-first table tool built on DuckDB, where every change is a visible, undoable operation and the engine does all the math with exact decimals.

- **Goals:** [docs/GOALS.md](docs/GOALS.md)
- **Design doc:** https://claude.ai/artifact/QQdXFgm7XTf5iW4VcSS8N9
- **Repo:** https://github.com/paul-wolf/tally-spread (product name Tally; package name `tally-spread`)

Status: early but usable. Sheets with your own columns, calculated columns, sections, rates and XLSX export work; AI, links-to-rows, taxes and PDF are next.

## What works now

- **Your own columns.** Columns are lettered A, B, C… and named as you like. Insert left or right, delete (undoable), drag to reorder, rename, change type. Each column has a type: **Text**, **Number**, **Link**, **Money** in one currency (e.g. a "UAH" column), or **Calculated**.
- **Calculated columns.** One formula for the whole column, using column names:
  - `coalesce(to(UAH, USD), USD)`: unit cost in USD from whichever price is filled in
  - `[Unit USD] * Qty`: line total
  - `to([Total USD], EUR)`: convert at the sheet's rates

  Functions: `to`, `coalesce`, `round`, `abs`, `min`, `max`, `if`. Amounts keep their currency: adding UAH to USD is an error, never a silent mix. Renaming a column updates the formulas that use it. There are no per-cell formulas.
- **Sections.** Any row can be a section heading; columns marked "Add up" get a subtotal on the heading and a sheet total.
- **Exchange rates.** ECB (preferred), NBU and HMRC monthly, updated automatically. ECB does not publish hryvnia, so NBU covers UAH. Per sheet: preferred source, live or locked to a date, and fixed rates of your own (e.g. 1 USD = 44.92 UAH) that win over every source. The rates used are listed under the totals.
- **Exact money**: decimal arithmetic, per-currency rounding; amounts typed as `₴12 499,50`, `$ 71.50`, `1,234.56` or `12.50 EUR`.
- **Typing and pasting like a spreadsheet.** Blank rows are always there to type into. Paste from Excel or Google Sheets anywhere (a heading row is skipped; values that don't fit a column are left out and reported). Copy, cut, transposed copy and paste, fill down, undo/redo of everything.
- **Sheets as tabs** (blank, or a spending-list template); **projects** as folders.
- **XLSX export** with real `=SUM()` subtotals and totals, clickable links and the rates used.
- **Two machines**: logs are per machine, so a synced folder (Syncthing) never has two writers per file; the app picks up the other machine's changes every few seconds.
- **CLI** (`tally`) for the always-on hub: daily rate updates, conversions, printing a sheet, SQL over a project, export.

## Layout

```
crates/tally-core   engine: money, rates, formulas, operations, projects (DuckDB), sheets, XLSX export
crates/tally-cli    `tally` command line
src-tauri           desktop app shell (Tauri 2)
ui                  React UI with a canvas grid (Glide Data Grid)
docs                goals and design notes
```

## Data on disk

```
~/Tally/                      (TALLY_HOME; sync this folder between machines)
  library/fx_rates/<machine>.jsonl
  projects/<project>/
    project.json              settings: currencies, rate sources, rounding, tax home
    ops/<machine>.jsonl       append-only log of operations
```

DuckDB holds the current state in memory, rebuilt from the logs when a project opens. Only the logs need syncing and backing up.

## Building

Prerequisites: Rust (stable), Node 20+, and on Linux the Tauri system libraries (`libwebkit2gtk-4.1-dev`, `libgtk-3-dev`, `librsvg2-dev`, `libayatana-appindicator3-dev`). The first build compiles DuckDB from source and takes several minutes.

```sh
npm install
npm run tauri dev        # run the app
npm run tauri build      # build a .app / .dmg (macOS) or .deb / .AppImage (Linux)
cargo test -p tally-core # engine tests
cargo install --path crates/tally-cli   # the `tally` command
```

## On the hub (always-on Linux machine)

```sh
tally rates update                      # HMRC monthly, NBU, ECB into ~/Tally/library
tally rates status
tally convert 12499.50 UAH GBP
tally show my-project
tally sql my-project "SELECT section, sum(total_usd) FROM spending GROUP BY 1"   # each sheet is a table
```

Run `tally rates update` daily, e.g. with cron: `15 7 * * * $HOME/.cargo/bin/tally rates update`.

## First-run defaults

A new project prefers ECB rates (NBU for hryvnia), keeps GBP as its reporting currency and the UK as tax home, and starts with a spending-list sheet. Change project settings in its `project.json`.

Projects made by the first skeleton version can't be opened; they are skipped.

## Keyboard

| Keys | Action |
| --- | --- |
| Arrows, Enter, Tab, typing | Move and edit; typing in a blank row starts a new row |
| ⌘C / ⌘X / ⌘V | Copy / cut / paste (tab-separated, works with Excel and Google Sheets) |
| ⇧⌘C / ⇧⌘V | Copy / paste transposed |
| ⌘D | Fill down |
| ⌘Z / ⇧⌘Z | Undo / redo |
| ⌘Enter | Insert a row above the selection |
| ⇧⌘S | Make the selected row a section heading (or back) |
| Delete | Clear cells, or delete selected rows |
| Click or right-click a column header | Edit the column: name, type, formula, insert, delete |
| Ctrl+PageUp / PageDown | Previous / next sheet |
| ⌘-click a link | Open it |

## License

MIT
