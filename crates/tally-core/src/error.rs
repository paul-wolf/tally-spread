use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid currency code: {0:?}")]
    InvalidCurrency(String),
    #[error("could not read {0:?} as an amount")]
    InvalidAmount(String),
    #[error("invalid value: {0}")]
    InvalidValue(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("could not parse {what}: {detail}")]
    Parse { what: String, detail: String },
    #[error("database error: {0}")]
    Db(#[from] duckdb::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("xlsx error: {0}")]
    Xlsx(#[from] rust_xlsxwriter::XlsxError),
    #[error("network error: {0}")]
    Network(String),
}

impl Error {
    pub(crate) fn parse(what: impl Into<String>, detail: impl ToString) -> Self {
        Error::Parse { what: what.into(), detail: detail.to_string() }
    }
}
