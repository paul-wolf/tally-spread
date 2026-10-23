# Tally

A fast, AI-driven tool for everyday money work: cost lists in any currency, totals in several, exchange rates and taxes handled for you.

Tally is not an Excel clone. It is a keyboard-first table tool built on DuckDB, where every change is a visible, undoable operation and the engine does all the math with exact decimals.

- **Goals:** [docs/GOALS.md](docs/GOALS.md)
- **Design doc:** https://claude.ai/artifact/QQdXFgm7XTf5iW4VcSS8N9
- **Repo:** https://github.com/paul-wolf/tally-spread (product name Tally; package name `tally-spread`)

Status: early but usable. Sheets with your own columns, calculated columns, sections, rates and export to XLSX, PDF, CSV and JSON work; AI, links-to-rows and taxes are next.

## What works now

- **Your own columns.** Every column has an invariant ID (`c1`, `c2`, … never reused), a display name (`Price (UAH)`) and a variable name for formulas and SQL (`price_uah`, derived from the display name unless you set it). Insert left or right, delete (undoable), drag to reorder, rename, change type. Values are **typed in** or **calculated**; types are **Text**, **Number**, **Link**, **Money** in one currency, **Date** and **Date & time** (stored in UTC, shown in local time; typed as `2026-09-18`, `18.09.2026`, `2026-09-18 13:46` or a Unix timestamp). A calculated **Money (USD)** column converts whatever its formula gives into USD at the sheet's rates, so a unit price in USD is just `coalesce(price_uah, price_usd)`. Changing a column's type converts the values already in it (e.g. pasted `UAH 3,250.00`).
- **Calculated columns.** One formula for the whole column; a column reference means that column on the same row:
  - `coalesce(to(price_uah, USD), price_usd)`: unit cost in USD from whichever price is filled in
  - `unit_usd * qty`: line total
  - `to(c8, EUR)` or `[Total (USD)] * 1.2`: columns by ID or display name work too

  Built-in functions: `to`, `coalesce`, `round`, `abs`, `min`, `max`, `if`, `date`, plus your own. Dates subtract to days and times to seconds: `(landed - took_off) / 60` is minutes, `bought + 30` is 30 days later. Currency codes are predefined names in capitals (`USD`, `UAH`); variable names are lowercase, so they never clash. Formulas are stored by column ID, so renaming never breaks one. Amounts keep their currency: adding UAH to USD is an error, never a silent mix. The column dialog completes names as you type and previews the result. Double-click any calculated value to see its column's formula. `coalesce()` flags rows where more than one of its values is filled in (say, a price typed in two currencies). There are no per-cell formulas.
- **Sections and totals.** Any row can be a section heading. Columns marked "Show total" get a subtotal on each heading and a total in the Total row pinned at the bottom of the grid.
- **Rate card per sheet.** Each currency gets exactly one rate against the sheet's base currency (e.g. USD), and every conversion goes through it, so a currency never has two rates in one sheet and cross rates always agree. Open it from the toolbar to see each rate's source, the date it applies to and when it was fetched. Rates float with the latest daily rates (ECB preferred; NBU covers hryvnia, which ECB does not publish; HMRC monthly available) or are pegged to a date; any rate can be pinned by hand.
- **Your own functions.** Define `vat(x) = x * 0.2` or `usd(x) = to(x, USD)` once (ƒ Functions) and use them in any formula in every sheet and project; they live in the shared library and sync between machines. Functions can use their parameters, numbers, currencies, built-ins and each other (loops are refused).
- **Exact money**: decimal arithmetic, per-currency rounding; amounts typed as `₴12 499,50`, `$ 71.50`, `1,234.56` or `12.50 EUR`.
- **Typing and pasting like a spreadsheet.** Blank rows are always there to type into. Paste from Excel or Google Sheets anywhere (a heading row is skipped; values that don't fit a column are left out and reported). Copy, cut, transposed copy and paste, fill down, undo/redo of everything.
- **Sheets as tabs** (blank, a spending-list template, or **from a file**: a CSV or JSON file becomes a sheet whose columns are named after its headings and typed from its values: numbers, dates, dates and times, Unix timestamps when the heading says so (`ts`, `time`, `created_at`), money in one currency, links, text); **projects** as folders.
- **Analyse** (toolbar or ⇧⌘A, on the selected columns): a drawer beside the grid with charts of values **over time** (against the sheet's date & time column; long logs are grouped into time buckets with the average as a line and lowest–highest as a band), **distributions** (histogram with a fitted normal curve, box plot, quartiles, outliers, skewness; counts for text columns) and **groups** (by a column, or by time every minute, hour, …: rows, mean, min, median, max). Charts save as PNG, figures copy as text, a group summary can be kept as a sheet. Nothing is added to the sheet itself.
- **Export** to XLSX (real `=SUM()` subtotals and totals, clickable links, the rates used), PDF (laid out like the sheet, with section subtotals, totals and rates), and CSV or JSON for other tools (one line per item, its section as a field).
- **Bold columns**: tick Bold in the column dialog; it carries into XLSX and PDF.
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
  library/functions/<machine>.jsonl
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
tally import my-project flight-log.csv          # a file as a new sheet
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
| ⇧⌘A | Analyse the selected columns |
| ⌘Z / ⇧⌘Z | Undo / redo |
| ⌘Enter | Insert a row above the selection |
| ⇧⌘S | Make the selected row a section heading (or back) |
| Delete | Clear cells, or delete selected rows |
| Double-click or right-click a column header | Edit the column: name, type, formula, style, insert, delete |
| Double-click a calculated value | Edit its column's formula |
| Ctrl+PageUp / PageDown | Previous / next sheet |
| ⌘-click a link | Open it |

## License

MIT
