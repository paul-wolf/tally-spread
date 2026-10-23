//! Tally's engine.
//!
//! Tally keeps money exact (decimal arithmetic, never floating point), keeps every
//! change as an append-only operation log, and stores the current state in DuckDB so
//! it can be queried with SQL. The UI and the AI both work by producing [`ops::Op`]s;
//! the engine applies them and does all the math.

pub mod analysis;
pub mod error;
pub mod export;
pub mod formats;
pub mod formula;
pub mod import;
pub mod fx;
pub mod home;
pub mod library;
pub mod money;
pub mod ops;
pub mod project;
pub mod sheet;
pub mod templates;
pub mod time;

pub use error::{Error, Result};
pub use money::{Currency, Rounding, RoundingScope};
pub use ops::{ColumnKind, ColumnSpec, Op, RateMode, RowKind};
pub use project::{Project, ProjectSettings};
