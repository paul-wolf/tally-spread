//! Currencies, exact amounts, parsing of typed or pasted amounts, and rounding.

use std::fmt;
use std::str::FromStr;

use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// An ISO 4217 currency code, always upper case (e.g. `UAH`, `GBP`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Currency(String);

impl Currency {
    pub fn new(code: &str) -> Result<Self> {
        let c = code.trim().to_ascii_uppercase();
        if c.len() == 3 && c.chars().all(|ch| ch.is_ascii_uppercase()) {
            Ok(Self(c))
        } else {
            Err(Error::InvalidCurrency(code.to_string()))
        }
    }

    pub fn code(&self) -> &str {
        &self.0
    }

    /// Number of decimal places used for amounts in this currency (ISO 4217 minor units).
    pub fn minor_units(&self) -> u32 {
        match self.0.as_str() {
            "BIF" | "CLP" | "DJF" | "GNF" | "ISK" | "JPY" | "KMF" | "KRW" | "PYG" | "RWF"
            | "UGX" | "UYI" | "VND" | "VUV" | "XAF" | "XOF" | "XPF" => 0,
            "BHD" | "IQD" | "JOD" | "KWD" | "LYD" | "OMR" | "TND" => 3,
            _ => 2,
        }
    }
}

impl fmt::Display for Currency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for Currency {
    type Error = Error;
    fn try_from(s: String) -> Result<Self> {
        Currency::new(&s)
    }
}

impl From<Currency> for String {
    fn from(c: Currency) -> String {
        c.0
    }
}

impl FromStr for Currency {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        Currency::new(s)
    }
}

/// Active ISO 4217 currency codes. In formulas these are predefined names
/// (written in capitals, e.g. `USD`).
pub const ISO_CODES: &[&str] = &[
    "AED", "AFN", "ALL", "AMD", "ANG", "AOA", "ARS", "AUD", "AWG", "AZN", "BAM", "BBD", "BDT", "BGN", "BHD", "BIF",
    "BMD", "BND", "BOB", "BRL", "BSD", "BTN", "BWP", "BYN", "BZD", "CAD", "CDF", "CHF", "CLP", "CNY", "COP", "CRC",
    "CUP", "CVE", "CZK", "DJF", "DKK", "DOP", "DZD", "EGP", "ERN", "ETB", "EUR", "FJD", "FKP", "GBP", "GEL", "GHS",
    "GIP", "GMD", "GNF", "GTQ", "GYD", "HKD", "HNL", "HTG", "HUF", "IDR", "ILS", "INR", "IQD", "IRR", "ISK", "JMD",
    "JOD", "JPY", "KES", "KGS", "KHR", "KMF", "KPW", "KRW", "KWD", "KYD", "KZT", "LAK", "LBP", "LKR", "LRD", "LSL",
    "LYD", "MAD", "MDL", "MGA", "MKD", "MMK", "MNT", "MOP", "MRU", "MUR", "MVR", "MWK", "MXN", "MYR", "MZN", "NAD",
    "NGN", "NIO", "NOK", "NPR", "NZD", "OMR", "PAB", "PEN", "PGK", "PHP", "PKR", "PLN", "PYG", "QAR", "RON", "RSD",
    "RUB", "RWF", "SAR", "SBD", "SCR", "SDG", "SEK", "SGD", "SHP", "SLE", "SOS", "SRD", "SSP", "STN", "SVC", "SYP",
    "SZL", "THB", "TJS", "TMT", "TND", "TOP", "TRY", "TTD", "TWD", "TZS", "UAH", "UGX", "USD", "UYU", "UZS", "VES",
    "VND", "VUV", "WST", "XAF", "XAG", "XAU", "XCD", "XDR", "XOF", "XPD", "XPF", "XPT", "YER", "ZAR", "ZMW", "ZWG",
];

pub fn is_iso_code(code: &str) -> bool {
    ISO_CODES.binary_search(&code).is_ok()
}

/// How amounts are rounded to a currency's minor units.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rounding {
    /// 0.5 rounds away from zero (usual for invoices and VAT).
    #[default]
    HalfUp,
    /// 0.5 rounds to the nearest even digit (banker's rounding).
    HalfEven,
}

/// Whether converted amounts are rounded per line (and the rounded lines summed),
/// or only the total is rounded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoundingScope {
    #[default]
    PerLine,
    OnTotal,
}

pub fn round(amount: Decimal, currency: &Currency, rounding: Rounding) -> Decimal {
    let strategy = match rounding {
        Rounding::HalfUp => RoundingStrategy::MidpointAwayFromZero,
        Rounding::HalfEven => RoundingStrategy::MidpointNearestEven,
    };
    amount.round_dp_with_strategy(currency.minor_units(), strategy)
}

/// Format an amount with exactly the currency's number of decimals.
pub fn format_amount(amount: Decimal, currency: &Currency) -> String {
    let mut a = amount;
    a.rescale(currency.minor_units());
    a.to_string()
}

const SYMBOLS: &[(&str, &str)] = &[
    ("грн.", "UAH"),
    ("грн", "UAH"),
    ("₴", "UAH"),
    ("£", "GBP"),
    ("€", "EUR"),
    ("us$", "USD"),
    ("$", "USD"),
    ("zł", "PLN"),
    ("¥", "JPY"),
    ("₣", "CHF"),
];

/// Read an amount as people type or paste it: `1 234,56`, `1,234.56`, `₴1200`,
/// `£12.50`, `12.50 EUR`, `(15.00)`. Returns the amount and any currency it named.
pub fn parse_amount(input: &str) -> Result<(Decimal, Option<Currency>)> {
    let err = || Error::InvalidAmount(input.to_string());
    let mut s: String = input
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '\u{a0}' | '\u{202f}' | '\u{2009}' | '\'' | '’'))
        .collect();
    if s.is_empty() {
        return Err(err());
    }

    let mut negative = false;
    if s.starts_with('(') && s.ends_with(')') {
        negative = true;
        s = s[1..s.len() - 1].to_string();
    }
    if let Some(rest) = s.strip_prefix('-') {
        negative = !negative;
        s = rest.to_string();
    }

    // Currency named by a symbol or a 3-letter code, before or after the number.
    // Work on the lowercased text so that matches and slices refer to the same string
    // (lowercasing can change a string's byte length), and slice only with `get`, which
    // returns `None` instead of panicking inside a multi-byte character (e.g. Cyrillic).
    let mut currency = None;
    s = s.to_lowercase();
    for (sym, code) in SYMBOLS {
        if let Some(rest) = s.strip_prefix(sym) {
            s = rest.to_string();
            currency = Some(Currency::new(code)?);
            break;
        }
        if let Some(rest) = s.strip_suffix(sym) {
            s = rest.to_string();
            currency = Some(Currency::new(code)?);
            break;
        }
    }
    if currency.is_none() && s.len() > 3 {
        fn code(part: Option<&str>) -> Option<&str> {
            part.filter(|p| p.chars().all(|c| c.is_ascii_alphabetic()))
        }
        if let (Some(head), Some(rest)) = (code(s.get(..3)), s.get(3..)) {
            currency = Some(Currency::new(head)?);
            s = rest.to_string();
        } else if let (Some(tail), Some(rest)) = (code(s.get(s.len() - 3..)), s.get(..s.len() - 3)) {
            currency = Some(Currency::new(tail)?);
            s = rest.to_string();
        }
    }
    if let Some(rest) = s.strip_prefix('-') {
        negative = !negative;
        s = rest.to_string();
    }

    // Scientific notation as written by loggers and pandas: `1.35e-05`, `2E3`.
    // Read exactly (no float round trip); only a dot may be the decimal separator.
    if let Some(d) = parse_scientific(&s) {
        let d = if negative { -d } else { d };
        return Ok((d.normalize(), currency));
    }

    if s.is_empty() || !s.chars().all(|c| c.is_ascii_digit() || c == ',' || c == '.') {
        return Err(err());
    }

    let commas = s.matches(',').count();
    let dots = s.matches('.').count();
    let normalized = match (commas, dots) {
        (0, 0) => s.clone(),
        (_, 0) => {
            // Only commas: one comma not followed by exactly 3 digits is a decimal comma.
            let after = s.len() - s.rfind(',').unwrap() - 1;
            if commas == 1 && after != 3 {
                s.replace(',', ".")
            } else {
                s.replace(',', "")
            }
        }
        (0, _) => {
            if dots == 1 {
                s.clone()
            } else {
                s.replace('.', "")
            }
        }
        _ => {
            // Both: whichever comes last is the decimal separator.
            if s.rfind(',') > s.rfind('.') {
                s.replace('.', "").replace(',', ".")
            } else {
                s.replace(',', "")
            }
        }
    };

    let mut d = Decimal::from_str(&normalized).map_err(|_| err())?;
    if negative {
        d = -d;
    }
    Ok((d.normalize(), currency))
}

/// `<digits>[.<digits>](e|E)[+|-]<digits>`, parsed exactly. `None` if `s` is not that shape
/// or the value does not fit in a `Decimal`.
fn parse_scientific(s: &str) -> Option<Decimal> {
    let (mantissa, exponent) = s.split_once(['e', 'E'])?;
    let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if int.is_empty() && frac.is_empty()
        || !int.chars().chain(frac.chars()).all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let digits = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let exp: i32 = exponent.parse().ok()?;
    if exp < -40 {
        return Some(Decimal::ZERO); // below what a Decimal can hold: effectively zero
    }
    if exp > 40 {
        return None; // too large for a Decimal
    }
    let int = if int.is_empty() { "0" } else { int };
    let frac = if frac.is_empty() { "0" } else { frac };
    let mut d = Decimal::from_str(&format!("{int}.{frac}")).ok()?;
    // Shifting by ten is exact within a Decimal's 28 places; beyond that it rounds.
    for _ in 0..exp.unsigned_abs() {
        d = if exp > 0 { d.checked_mul(Decimal::TEN)? } else { d.checked_div(Decimal::TEN)? };
    }
    Some(d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn p(s: &str) -> (Decimal, Option<String>) {
        let (d, c) = parse_amount(s).unwrap();
        (d, c.map(|c| c.code().to_string()))
    }

    #[test]
    fn parses_common_formats() {
        assert_eq!(p("1234.56"), (dec!(1234.56), None));
        assert_eq!(p("1 234,56"), (dec!(1234.56), None));
        assert_eq!(p("1,234.56"), (dec!(1234.56), None));
        assert_eq!(p("1.234,56"), (dec!(1234.56), None));
        assert_eq!(p("1,234"), (dec!(1234), None));
        assert_eq!(p("12,5"), (dec!(12.5), None));
        assert_eq!(p("1.234.567"), (dec!(1234567), None));
        assert_eq!(p("(15.00)"), (dec!(-15), None));
        assert_eq!(p("-3"), (dec!(-3), None));
    }

    #[test]
    fn parses_scientific_notation() {
        assert_eq!(p("1.3552957170759328e-05"), (dec!(0.000013552957170759328), None));
        assert_eq!(p("-2.5E3"), (dec!(-2500), None));
        assert_eq!(p("1e+2"), (dec!(100), None));
        assert_eq!(p(".5e1"), (dec!(5), None));
        assert_eq!(p("3e0 USD"), (dec!(3), Some("USD".into())));
        assert!(parse_amount("1e").is_err());
        assert!(parse_amount("e5").is_err());
        assert!(parse_amount("1,5e3").is_err());
    }

    #[test]
    fn detects_currencies() {
        assert_eq!(p("₴1 200"), (dec!(1200), Some("UAH".into())));
        assert_eq!(p("1 200 грн"), (dec!(1200), Some("UAH".into())));
        assert_eq!(p("£12.50"), (dec!(12.5), Some("GBP".into())));
        assert_eq!(p("12.50 EUR"), (dec!(12.5), Some("EUR".into())));
        assert_eq!(p("usd 99"), (dec!(99), Some("USD".into())));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_amount("").is_err());
        assert!(parse_amount("abc").is_err());
        assert!(parse_amount("12a4").is_err());
    }

    #[test]
    fn rejects_non_ascii_text_without_panicking() {
        // Bank statements are full of Cyrillic descriptions; these used to panic by
        // slicing inside a multi-byte character.
        for s in ["Кафе", "Оплата товарів", "Переказ на картку", "Ab€c", "1€x", "aбв", "ГРН", "İİİİ 12"] {
            assert!(parse_amount(s).is_err(), "{s}");
        }
        assert_eq!(p("1 200 ГРН"), (dec!(1200), Some("UAH".into())));
        assert_eq!(p("1 200,50 грн."), (dec!(1200.5), Some("UAH".into())));
    }

    #[test]
    fn rounds_per_currency() {
        let gbp = Currency::new("gbp").unwrap();
        let jpy = Currency::new("JPY").unwrap();
        assert_eq!(round(dec!(2.345), &gbp, Rounding::HalfUp), dec!(2.35));
        assert_eq!(round(dec!(2.345), &gbp, Rounding::HalfEven), dec!(2.34));
        assert_eq!(round(dec!(1234.5), &jpy, Rounding::HalfUp), dec!(1235));
        assert_eq!(format_amount(dec!(3), &gbp), "3.00");
    }

    #[test]
    fn iso_list_is_sorted() {
        assert!(ISO_CODES.windows(2).all(|w| w[0] < w[1]));
        assert!(is_iso_code("UAH") && is_iso_code("USD") && !is_iso_code("usd") && !is_iso_code("QTY"));
    }

    #[test]
    fn currency_validation() {
        assert!(Currency::new("uah").is_ok());
        assert!(Currency::new("UA").is_err());
        assert!(Currency::new("U4H").is_err());
    }
}
