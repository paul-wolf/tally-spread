# Tally

A fast, AI-driven tool for everyday money work: cost lists in any currency, totals in several, exchange rates and taxes handled for you.

Tally is not an Excel clone. It is a keyboard-first table tool built on DuckDB, where every change is a visible, undoable operation and the engine does all the math with exact decimals.

- **Goals:** [docs/GOALS.md](docs/GOALS.md)
- **Design doc:** https://claude.ai/artifact/QQdXFgm7XTf5iW4VcSS8N9
- **Repo:** https://github.com/paul-wolf/tally-spread (product name Tally; package name `tally-spread`)

Status: first skeleton. Cost lists work end to end; AI, links-to-rows, taxes and PDF are next.

## What works now

- **Cost lists**: item, link, quantity, unit price and currency per row; rows in any mix of currencies.
- **Display currencies per sheet** (e.g. `GBP, UAH`), each with a converted column and a total.
- **Exchange rates** from HMRC (monthly, for UK records), the National Bank of Ukraine and the ECB, plus rates entered by hand. Sheets use **live** rates or rates **locked** to a date. Conversion picks a path through the rates in force on that date, preferring one source.
- **Exact money**: decimal arithmetic, per-currency rounding (half-up by default), amounts typed as `₴12 499,50`, `£12.50`, `1,234.56` or `12.50 EUR` (a currency typed with the price sets the row's currency).
- **Undo/redo** of everything; every change is an operation in an append-only log.
- **Copy/paste** with Excel and Google Sheets (tab-separated), paste that adds rows, **transposed** copy and paste, fill down.
- **Sheets as tabs**; **projects** as folders.
- **XLSX export** with live `=SUM()` totals, clickable links and the rates used.
- **Two machines**: logs are per machine, so a synced folder (Syncthing) never has two writers per file; the app picks up the other machine's changes every few seconds.
- **CLI** (`tally`) for the always-on hub: daily rate updates, conversions, SQL over a project.

## Layout

```
crates/tally-core   engine: money, rates, operations, projects (DuckDB), cost lists, XLSX export
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
tally show my-costs
tally sql my-costs "SELECT currency, sum(unit_price * coalesce(qty, 1)) FROM cost_items WHERE NOT deleted GROUP BY 1"
```

Run `tally rates update` daily, e.g. with cron: `15 7 * * * $HOME/.cargo/bin/tally rates update`.

## First-run defaults

A new project starts with GBP as the reporting currency, `GBP, UAH` as display currencies, rate sources in the order HMRC monthly, NBU, ECB, manual, and the UK as tax home. Change them in the project's `project.json`.

## Keyboard

| Keys | Action |
| --- | --- |
| Arrows, Enter, Tab, typing | Move and edit |
| ⌘C / ⌘V | Copy / paste (tab-separated, works with Excel and Google Sheets) |
| ⇧⌘C / ⇧⌘V | Copy / paste transposed |
| ⌘D | Fill down |
| ⌘Z / ⇧⌘Z | Undo / redo |
| Delete | Clear cells, or delete selected rows |
| Ctrl+PageUp / PageDown | Previous / next sheet |
| ⌘-click a link | Open it |

## License

MIT
