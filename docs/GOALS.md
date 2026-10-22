# Tally: Project Goals

Check every proposal and design against these goals and flag any conflict.

1. **Be fast for everyday money work.** Summing columns, calculations across columns and cross tabs in a fluid, keyboard-first interface, with excellent copy, paste and transpose, including clean pasting to and from Excel.
2. **Get multi-currency right.** Currency is a built-in part of every amount, all arithmetic uses exact decimals, and conversions are explicit, with known rates and dates, so different currencies are never silently added together.
3. **Make AI a first-class way to work.** Plain-language requests become visible, undoable operations; the engine does all the math. Routine actions stay cheap through a local command parser, reusable recipes, a small default model and a budget cap.
4. **Keep the data open through an existing engine.** DuckDB stores the data, so SQL, Python, pandas, Polars, Arrow and an MCP server for agents all work without building custom interfaces.
5. **Move data in and out easily.** Import Excel workbooks (XLSX and older XLS), CSV/TSV, JSON and Parquet, bringing in values and sheets rather than Excel formatting. Export XLSX with live totals, and polished PDF reports.
6. **Run locally, cross-platform, as open source.** Runs on macOS and Linux under the MIT license; data stays on the user's own machines; only AI requests go to a paid remote model.
7. **Work across two machines.** The Mac is a fast local workstation; the always-on Linux machine is the hub for sync, backups, exchange rates, scheduled jobs and the AI gateway, all inside the user's Tailscale network.
8. **Remember what you use repeatedly.** Projects scope the data; a reference library keeps products, exchange rates, tax rates and other repeated values with their history, so nothing has to be looked up twice.
9. **Keep cross-border purchases tax-ready (key feature).** Buy in one country and currency, keep records in another: official tax-authority exchange rates, VAT handled per country, actual bank amounts matched to purchases, and exports an accountant can check.

**Deliberately out of scope:** building a formula engine, full Excel compatibility, charts and macros.

## Primary use case and first milestone: cost lists

The most frequent task is writing lists of things that cost money, often with product URLs, priced in one or more currencies and shown in one or more display currencies. The first milestone centres on this: mixed-currency rows, multiple display currencies with totals, paste a product link to fill in name/price/currency, exchange rates fetched daily and managed automatically (live or locked to a date), and PDF/XLSX export with clickable links.

## Key feature: cross-border purchases and tax records

Purchases are made mainly in Ukraine (UAH, prices normally include Ukrainian VAT) while taxes are paid in the United Kingdom (GBP). Each project sets a tax home, reporting currency and rate source (e.g. UK · GBP · HMRC monthly rates, fetched automatically by the hub; alternatives: HMRC yearly average or actual rate paid). Ukrainian VAT is recorded as part of the cost, not as reclaimable UK VAT. Bank statements can be imported and matched to purchases so the actual GBP paid replaces estimates. Rows keep link, captured price, date and optional receipt; exports state rate source, dates and tax treatment. The app calculates and documents; the accountant decides which treatment applies.

## Core concepts

- **Global library → Projects → Sheets.**
- **Projects** scope the data: sheets, project library entries, settings (currencies, live/locked rates, rounding, tax home, rate source) and AI notes. Lookups search the current project first, then global. A project is a folder on disk.
- **Sheets** are tabs: data sheets (typed in) and view sheets (live summaries/cross tabs over other sheets).
- **Reference library**: entries with a source and dated values; pasted links saved automatically; autocomplete everywhere; live or locked per row. Exchange and tax rates are global.
- **Taxes** are reference data: rates by country, category and date; each row knows tax-inclusive or exclusive and its rate; net, tax and gross shown in every display currency; tax computed in original currency, then converted.

Full design doc: https://claude.ai/artifact/QQdXFgm7XTf5iW4VcSS8N9
