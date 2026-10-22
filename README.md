# Tally

A fast, AI-driven tool for everyday money work: cost lists in any currency, totals in several, exchange rates and taxes handled for you.

Tally is not an Excel clone. It is a keyboard-first table tool built on DuckDB, where the AI proposes visible, undoable operations and the engine does all the math. It imports Excel, CSV, JSON and Parquet, and exports XLSX and polished PDFs.

- **Goals:** [docs/GOALS.md](docs/GOALS.md)
- **Design doc:** https://claude.ai/artifact/QQdXFgm7XTf5iW4VcSS8N9

Status: design stage, no code yet.

## Planned stack

| Part | Choice |
| --- | --- |
| Engine and storage | DuckDB, Parquet snapshots plus an append-only operation log |
| App | Tauri (Rust backend, web UI), macOS and Linux |
| Grid | Canvas-based grid (e.g. Glide Data Grid) |
| XLSX export | rust_xlsxwriter |
| PDF export | Typst |
| AI | Remote model through a cost-aware gateway, local command parser first |
| Sync | Syncthing over Tailscale, Linux machine as always-on hub |

## License

MIT
