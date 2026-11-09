# Tally: Project Goals

Check every proposal and design against these goals and flag any conflict.

1. **Be fast for everyday money work.** Summing columns, calculations across columns and cross tabs in a fluid, keyboard-first interface, with excellent copy, paste and transpose, including clean pasting to and from Excel. Sheets have the user's own columns, not a fixed layout.
2. **Get multi-currency right.** Currency is a built-in part of every amount, all arithmetic uses exact decimals, and conversions are explicit, with known rates and dates, so different currencies are never silently added together.
3. **Make AI a first-class way to work.** Plain-language requests become visible, undoable operations; the engine does all the math. Routine actions stay cheap through a local command parser, reusable recipes, a small default model and a budget cap.
4. **Keep the data open through an existing engine.** DuckDB stores the data, so SQL, Python, pandas, Polars, Arrow and an MCP server for agents all work without building custom interfaces.
5. **Move data in and out easily.** Import Excel workbooks (XLSX and older XLS), CSV/TSV, JSON and Parquet, bringing in values and sheets rather than Excel formatting. Export XLSX with live totals, and polished PDF reports.
6. **Run locally, cross-platform, as open source.** Runs on macOS and Linux under the MIT license; data stays on the user's own machines; only AI requests go to a paid remote model.
7. **Work across two machines.** The Mac is a fast local workstation; the always-on Linux machine is the hub for sync, backups, exchange rates, scheduled jobs and the AI gateway, all inside the user's Tailscale network.
8. **Remember what you use repeatedly.** Projects scope the data; a reference library keeps products, exchange rates, tax rates and other repeated values with their history, so nothing has to be looked up twice.
9. **Keep cross-border purchases tax-ready (key feature).** Buy in one country and currency, keep records in another: official tax-authority exchange rates, VAT handled per country, actual bank amounts matched to purchases, and exports an accountant can check.

**Deliberately out of scope:** building a formula engine, full Excel compatibility, charts and macros.

## Sheet model (decided 2026-10-23)

- **Flexible columns.** Columns are lettered A, B, C… like Excel and named per sheet; they can be inserted, deleted, moved, renamed and retyped at any time. Types: text, number, link, money in one currency (e.g. separate "UAH" and "USD" price columns), calculated.
- **Calculated columns, not cell formulas.** One formula per column (e.g. `coalesce(to(UAH, USD), USD)`, `[Unit USD] * Qty`), evaluated with exact decimals; literal values and formulas never mix in one column. Free-form per-cell formulas are not planned; if they turn out to be needed, embed IronCalc rather than write an engine.
- **Sections with subtotals** replace blank-row groups and hand-built subtotal cells.
- **Column naming (decided 2026-10-23):** every column has an invariant ID (c1, c2… never reused), a display name ("Price (USD)") and a variable name ("price_usd"). Formulas refer to columns by variable name, ID or [display name], are same-row, and are stored by ID so renames never break them. Row margin shows plain positions.
- **Rate card (decided 2026-10-23):** one rate per currency per sheet against a base currency, all conversions through it; viewable with source, rate date and fetch time; floating (latest daily) or pegged (date or pinned values). Per-row purchase-date rates are a later option.
- **User functions (decided 2026-10-23):** defined by the user as formulas over parameters (e.g. `vat(x) = x * 0.2`), stored in the shared library, usable in every sheet and project.
- **Excel import (planned):** pasting brings values only; importing an .xlsx will recognise repeating row formulas and turn them into calculated columns.
- **Values vs type (decided 2026-10-23):** a column's values are typed in or calculated; its type is text, number, money (currency) or link. A calculated Money (USD) column converts its results to USD at the rate card, so `coalesce(price_uah, price_usd)` needs no `to()`.
- **Currency names (decided 2026-10-23):** ISO codes are predefined names in capitals (`USD`); variable names are lowercase; no special cases.
- **Guiding principle:** lean on implied semantics. Most spreadsheet work is repetitive, so Tally should infer what it can (currency from the column, names from the type, rate-card base from the formulas) and ask only when it can't.
- **Rates:** the user chooses the provider per sheet; ECB preferred. ECB does not publish UAH, so NBU fills that gap automatically. Fixed hand-set rates per sheet (e.g. 1 USD = 44.92 UAH) win over every source.
- **Typical spending layout:** Item | Link | Note | UAH | USD | Unit USD (calc) | Qty | Total USD (calc, totalled). Spending lists of this kind are in USD and unrelated to tax; the GBP/HMRC setup applies to tax work only.
- **Lesson (2026-10-23):** typed columns work and save a lot of time; keep the structure.
- **Analysis (decided 2026-10-23):** charts are for looking, not for storing in sheets: a drawer beside the grid shows values over time (bucketed for long logs, min–max band), distributions (histogram, normal fit, quartiles) and group summaries. Charts save as PNG; a summary can be kept as a sheet.
- **Next ideas:** sheet types. Today's sheet is the table type (a dataframe or SQL table) and the basis for the others. Candidates are a summary or cross-tab sheet built from table sheets, and a ledger whose formulas can read the previous row (a running balance).

## Primary use case and first milestone: cost lists

The most frequent task is writing lists of things that cost money, often with product URLs, priced in one or more currencies and shown in one or more display currencies. Done so far: user-defined columns, calculated columns, sections, rates fetched and managed automatically (live, locked to a date, or fixed), paste from Excel, XLSX export. Still to do in this milestone: paste a product link to fill in name/price/currency, PDF export.

## Key feature: cross-border purchases and tax records

Purchases are made mainly in Ukraine (UAH, prices normally include Ukrainian VAT) while taxes are paid in the United Kingdom (GBP). Each project sets a tax home, reporting currency and rate source (e.g. UK · GBP · HMRC monthly rates, fetched automatically by the hub; alternatives: HMRC yearly average or actual rate paid). Ukrainian VAT is recorded as part of the cost, not as reclaimable UK VAT. Bank statements can be imported and matched to purchases so the actual GBP paid replaces estimates. Rows keep link, captured price, date and optional receipt; exports state rate source, dates and tax treatment. The app calculates and documents; the accountant decides which treatment applies.

## Core concepts

- **Global library → Projects → Sheets, with sheets first (decided 2026-11-06).** Starting a sheet never asks where it goes. New sheets (⌘N, ⇧⌘N from the clipboard, imports) go to **Unfiled**, a hidden project with USD records and no tax home, unless started inside a project; they are filed later with Move to project (the tab's right-click menu). There is no current project to choose: tabs show sheets from any project, and the shown tab's project is the active one for undo, redo and settings. **Home** lists every sheet in every project (Recent, Timeline, Projects, Kind; search; archived sheets to restore), and ⌘K goes to any sheet by name. Plan and status: docs/PLAN-home.md.
- **Projects** group sheets that share settings (currencies, live/locked rates, rounding, tax home, rate source), project library entries and AI notes, and are where summaries across sheets will live. Lookups search the sheet's own project first, then global. A project is a folder on disk.
- **Sheets** open as tabs from any project: data sheets (typed in) and view sheets (live summaries/cross tabs over other sheets). Closing a tab never archives or deletes a sheet.
- **Reference library**: entries with a source and dated values; pasted links saved automatically; autocomplete everywhere; live or locked per row. Exchange and tax rates are global. (The sheet list is called Home, so "Library" always means this.)
- **Taxes** are reference data: rates by country, category and date; each row knows tax-inclusive or exclusive and its rate; net, tax and gross shown in every display currency; tax computed in original currency, then converted.

## Proposals (2026-11-06, not yet decided)

From a review of GridCraft (github.com/storytold/gridcraft, an open-source Rust Excel clone, MIT/Apache-2.0, commit 6a8eb4f). We take ideas only; the code is Excel-compatible Rust and not reused.

### A. Formula scopes: this row, previous row, whole column or section

**Conflict:** this changes the sheet model decision that "formulas are same-row". Adopting it amends that decision. It keeps one formula per column and needs no formula engine.

- **What:** a calculated column's formula can read three scopes:
  - *This row* (today): `[Unit USD] * qty`.
  - *Previous row:* `prev(x)` is the value of column `x` in the row above, e.g. `prev(balance) + amount`. This covers the ledger sheet idea above.
  - *Whole column or section:* `sum`, `avg`, `min`, `max` and `count` over a column, for the whole sheet or only the row's section, e.g. `total_usd / sum(total_usd)` (share of total), `price_usd - avg(price_usd, section)` (difference from the section average).
- **Engine:** these map to DuckDB window functions, so the engine still does the maths (goal 4): `sum(x) OVER ()`, `sum(x) OVER (PARTITION BY section)`, `lag(x) OVER (ORDER BY row_pos)`. A column that reads its own previous value (`balance = prev(balance) + amount`) is not a plain `lag`. Its common form becomes a cumulative sum (`sum(amount) OVER (ORDER BY row_pos ROWS UNBOUNDED PRECEDING)`); any other self-reference is evaluated row by row in order with exact decimals.
- **Currency and decimals:** unchanged rules (goal 2). An aggregate of a Money (USD) column is USD; aggregates use exact decimals.
- **Filters:** aggregates inside formulas cover all rows, so stored values never change when a view is filtered. Totals shown on screen may count only visible rows; that is a display choice and separate from this.
- **Cycles:** a column cannot aggregate itself (`sum(x)` in `x`'s own formula). It is refused like any other cycle. `prev(x)` in `x`'s own formula is allowed.
- **Recalculation:** a formula with an aggregate depends on the whole column (or section), so a change recalculates that whole column.
- **Open questions:**
  - What `prev()` gives on the first row of a sheet or section (empty, so `coalesce(prev(balance), 0) + amount`?).
  - Whether `prev()` follows the stored row order or a sort column. A view sort must not change stored values; a ledger probably orders by date.
  - The syntax for section scope (`avg(x, section)` or something else).
  - Whether to offer `cumsum(x)` by name as well.

### B. Live statistics for the selected cells

- **What:** when more than one cell is selected, the status bar shows Sum, Average and Count by default. Min, Max and a count of numeric cells can be switched on; the choice is a user preference. The figures update as the selection changes, from the keyboard as well as the mouse (goal 1).
- **Currency (goal 2):**
  - Money in one currency is shown in that currency: `Sum $1,234.50`.
  - Mixed currencies are never added silently. The bar shows one sum per currency (`UAH 12,400.00 · USD 310.00`), optionally followed by a converted total in the sheet's base currency, clearly labelled: `≈ USD 612.40 at rate card`.
  - Plain numbers are summed apart from money. Text and link cells count only towards Count.
- **Decimals:** totals use exact decimals; averages are rounded only for display.
- **Actions:** a figure can be copied with a click or shortcut. Later: turn a selection's figure into a section subtotal or a summary sheet.
- **Cost:** nothing is stored and no formula is created. It is a query over the selected range.

### Other candidates from the same review (not yet written up)

- **A total function per column for section footers:** sum, average, count, min or max. Rows hidden by a filter are left out, and subtotals are never counted twice in a grand total (Excel's SUBTOTAL rules).
- **Apply an operation to selected literal values** (×1.2, +10%) as one undoable edit. This is also a natural target for AI requests.
- **A per-currency rounding rule:** round each row to the minor unit before totalling, or round only the total. Exports state which one was used, so totals match the bank and the accountant (goal 9).
- **Multi-step actions as one undo step:** every action is a named command, and several commands can run as one undo step that rolls back if any fails, so an AI request with several steps undoes in one go.

Full design doc: https://claude.ai/artifact/QQdXFgm7XTf5iW4VcSS8N9

Code repository: https://github.com/paul-wolf/tally-spread (local clone: ~/prj/tally on the Mac, branch main); docs/GOALS.md there mirrors this file. Product name: Tally; package/repo identifier: tally-spread.
