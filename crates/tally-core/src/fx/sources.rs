//! Parsers (and, with the `fetch` feature, downloaders) for official rate sources.
//!
//! - ECB: daily reference rates, `1 EUR = rate X`.
//! - NBU: National Bank of Ukraine official rates, `1 X = rate UAH`.
//! - HMRC: monthly rates for customs and VAT, `1 GBP = rate X`, valid for one month.

use std::str::FromStr;

use chrono::NaiveDate;
use rust_decimal::Decimal;

use super::{RateRecord, RateSource};
use crate::money::Currency;
use crate::{Error, Result};

pub const ECB_DAILY_URL: &str = "https://www.ecb.europa.eu/stats/eurofxref/eurofxref-daily.xml";
pub const ECB_90_DAYS_URL: &str = "https://www.ecb.europa.eu/stats/eurofxref/eurofxref-hist-90d.xml";
pub const NBU_URL: &str = "https://bank.gov.ua/NBUStatService/v1/statdirectory/exchange?json";

/// HMRC monthly CSV for a month (the month is not zero-padded in the file name).
pub fn hmrc_monthly_url(year: i32, month: u32) -> String {
    format!("https://trade-tariff.service.gov.uk/uk/api/exchange_rates/files/monthly_csv_{year}-{month}.csv")
}

fn decimal(s: &str, what: &str) -> Result<Decimal> {
    let s = s.trim();
    Decimal::from_str(s)
        .or_else(|_| Decimal::from_scientific(s))
        .map_err(|e| Error::parse(what, format!("{s:?}: {e}")))
}

fn date(s: &str, what: &str) -> Result<NaiveDate> {
    let s = s.trim();
    for fmt in ["%Y-%m-%d", "%d/%m/%Y", "%d.%m.%Y", "%d %b %Y", "%d %B %Y", "%d-%b-%Y", "%d-%m-%Y"] {
        if let Ok(d) = NaiveDate::parse_from_str(s, fmt) {
            return Ok(d);
        }
    }
    Err(Error::parse(what, format!("unrecognised date {s:?}")))
}

/// Value of `name='…'` or `name="…"` inside one XML tag.
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = tag;
    while let Some(i) = rest.find(name) {
        let before_ok = i == 0 || rest.as_bytes()[i - 1].is_ascii_whitespace();
        let after = rest[i + name.len()..].trim_start();
        if before_ok {
            if let Some(after_eq) = after.strip_prefix('=') {
                let after_eq = after_eq.trim_start();
                let quote = after_eq.chars().next()?;
                if quote == '\'' || quote == '"' {
                    let body = &after_eq[1..];
                    return body.find(quote).map(|end| &body[..end]);
                }
            }
        }
        rest = &rest[i + name.len()..];
    }
    None
}

/// ECB `eurofxref-daily.xml` or `eurofxref-hist*.xml`.
pub fn parse_ecb_xml(xml: &str) -> Result<Vec<RateRecord>> {
    let eur = Currency::new("EUR")?;
    let mut out = Vec::new();
    let mut current: Option<NaiveDate> = None;
    for chunk in xml.split('<').skip(1) {
        let Some(tag) = chunk.split('>').next() else { continue };
        if !tag.starts_with("Cube") {
            continue;
        }
        if let Some(t) = attr(tag, "time") {
            current = Some(date(t, "ECB date")?);
        }
        if let (Some(cur), Some(rate)) = (attr(tag, "currency"), attr(tag, "rate")) {
            let Some(d) = current else {
                return Err(Error::parse("ECB rates", "rate before any date"));
            };
            out.push(RateRecord {
                date: d,
                valid_to: None,
                base: eur.clone(),
                quote: Currency::new(cur)?,
                rate: decimal(rate, "ECB rate")?,
                source: RateSource::Ecb,
            });
        }
    }
    if out.is_empty() {
        return Err(Error::parse("ECB rates", "no rates found"));
    }
    Ok(out)
}

/// NBU `statdirectory/exchange?json`: `[{"cc":"USD","rate":41.4,"exchangedate":"21.10.2026"}, …]`.
pub fn parse_nbu_json(json: &str) -> Result<Vec<RateRecord>> {
    let uah = Currency::new("UAH")?;
    let items: Vec<serde_json::Value> = serde_json::from_str(json)?;
    let mut out = Vec::new();
    for item in items {
        let (Some(cc), Some(rate), Some(day)) = (
            item.get("cc").and_then(|v| v.as_str()),
            item.get("rate"),
            item.get("exchangedate").and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        // NBU also lists metals (XAU, XAG…) and SDR (XDR); keep them, they are harmless.
        let Ok(base) = Currency::new(cc) else { continue };
        out.push(RateRecord {
            date: date(day, "NBU date")?,
            valid_to: None,
            base,
            quote: uah.clone(),
            rate: decimal(&rate.to_string(), "NBU rate")?,
            source: RateSource::Nbu,
        });
    }
    if out.is_empty() {
        return Err(Error::parse("NBU rates", "no rates found"));
    }
    Ok(out)
}

/// HMRC monthly CSV. Columns are found by header name, so column order and
/// the date format may vary.
pub fn parse_hmrc_csv(csv_text: &str) -> Result<Vec<RateRecord>> {
    let gbp = Currency::new("GBP")?;
    let mut rdr = csv::ReaderBuilder::new()
        .flexible(true)
        .trim(csv::Trim::All)
        .from_reader(csv_text.trim_start_matches('\u{feff}').as_bytes());
    let headers: Vec<String> = rdr
        .headers()
        .map_err(|e| Error::parse("HMRC header", e))?
        .iter()
        .map(|h| h.to_lowercase())
        .collect();
    let find = |pred: &dyn Fn(&str) -> bool, name: &str| {
        headers
            .iter()
            .position(|h| pred(h))
            .ok_or_else(|| Error::parse("HMRC header", format!("no {name} column in {headers:?}")))
    };
    let code_i = find(&|h| h.contains("code"), "currency code")?;
    let rate_i = find(&|h| h.contains("per") || h.contains("rate"), "rate")?;
    let start_i = find(&|h| h.contains("start"), "start date")?;
    let end_i = find(&|h| h.contains("end"), "end date")?;

    let mut out = Vec::new();
    for row in rdr.records() {
        let row = row.map_err(|e| Error::parse("HMRC row", e))?;
        let get = |i: usize| row.get(i).unwrap_or("").trim();
        if get(code_i).is_empty() {
            continue;
        }
        let Ok(quote) = Currency::new(get(code_i)) else { continue };
        out.push(RateRecord {
            date: date(get(start_i), "HMRC start date")?,
            valid_to: Some(date(get(end_i), "HMRC end date")?),
            base: gbp.clone(),
            quote,
            rate: decimal(get(rate_i), "HMRC rate")?,
            source: RateSource::HmrcMonthly,
        });
    }
    if out.is_empty() {
        return Err(Error::parse("HMRC rates", "no rates found"));
    }
    Ok(out)
}

/// Parse a downloaded or hand-supplied file for a given source.
pub fn parse(source: RateSource, text: &str) -> Result<Vec<RateRecord>> {
    match source {
        RateSource::Ecb => parse_ecb_xml(text),
        RateSource::Nbu => parse_nbu_json(text),
        RateSource::HmrcMonthly => parse_hmrc_csv(text),
        RateSource::Manual => Err(Error::InvalidValue("manual rates are entered, not parsed".into())),
    }
}

#[cfg(feature = "fetch")]
pub mod fetch {
    //! Downloads from the official sources. Each function returns parsed records.

    use chrono::{Datelike, NaiveDate};

    use super::*;

    fn get(url: &str) -> Result<String> {
        let mut resp = ureq::get(url)
            .header("User-Agent", concat!("tally/", env!("CARGO_PKG_VERSION")))
            .call()
            .map_err(|e| Error::Network(format!("{url}: {e}")))?;
        resp.body_mut()
            .read_to_string()
            .map_err(|e| Error::Network(format!("{url}: {e}")))
    }

    /// Today's ECB reference rates.
    pub fn ecb_daily() -> Result<Vec<RateRecord>> {
        parse_ecb_xml(&get(ECB_DAILY_URL)?)
    }

    /// The last 90 days of ECB reference rates.
    pub fn ecb_90_days() -> Result<Vec<RateRecord>> {
        parse_ecb_xml(&get(ECB_90_DAYS_URL)?)
    }

    /// NBU official rates for a date (today if `None`).
    pub fn nbu(day: Option<NaiveDate>) -> Result<Vec<RateRecord>> {
        let url = match day {
            Some(d) => format!("{NBU_URL}&date={}", d.format("%Y%m%d")),
            None => NBU_URL.to_string(),
        };
        parse_nbu_json(&get(&url)?)
    }

    /// HMRC monthly rates for the month containing `day`.
    pub fn hmrc_month(day: NaiveDate) -> Result<Vec<RateRecord>> {
        parse_hmrc_csv(&get(&hmrc_monthly_url(day.year(), day.month()))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    const ECB: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<gesmes:Envelope xmlns:gesmes="http://www.gesmes.org/xml/2002-08-01" xmlns="http://www.ecb.int/vocabulary/2002-08-01/eurofxref">
	<gesmes:subject>Reference rates</gesmes:subject>
	<Cube>
		<Cube time='2026-10-21'>
			<Cube currency='USD' rate='1.1612'/>
			<Cube currency='GBP' rate='0.86750'/>
			<Cube currency='PLN' rate='4.2510'/>
		</Cube>
	</Cube>
</gesmes:Envelope>"#;

    const NBU: &str = r#"[{"r030":840,"txt":"Долар США","rate":41.7452,"cc":"USD","exchangedate":"22.10.2026"},
{"r030":978,"txt":"Євро","rate":48.4823,"cc":"EUR","exchangedate":"22.10.2026"},
{"r030":826,"txt":"Фунт стерлінгів","rate":55.8811,"cc":"GBP","exchangedate":"22.10.2026"}]"#;

    const HMRC: &str = "Country/Territories,Currency,Currency Code,Currency Units per £1,Start date,End date\n\
Ukraine,Hryvnia,UAH,58.0164,01/02/2026,28/02/2026\n\
USA,Dollar,USD,1.3410,01/02/2026,28/02/2026\n\
Eurozone,Euro,EUR,1.1448,01/02/2026,28/02/2026\n";

    #[test]
    fn ecb() {
        let r = parse_ecb_xml(ECB).unwrap();
        assert_eq!(r.len(), 3);
        assert_eq!(r[1].base.code(), "EUR");
        assert_eq!(r[1].quote.code(), "GBP");
        assert_eq!(r[1].rate, dec!(0.86750));
        assert_eq!(r[1].date.to_string(), "2026-10-21");
    }

    #[test]
    fn nbu() {
        let r = parse_nbu_json(NBU).unwrap();
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].base.code(), "USD");
        assert_eq!(r[0].quote.code(), "UAH");
        assert_eq!(r[0].rate, dec!(41.7452));
        assert_eq!(r[0].date.to_string(), "2026-10-22");
    }

    #[test]
    fn hmrc() {
        let r = parse_hmrc_csv(HMRC).unwrap();
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].base.code(), "GBP");
        assert_eq!(r[0].quote.code(), "UAH");
        assert_eq!(r[0].rate, dec!(58.0164));
        assert_eq!(r[0].date.to_string(), "2026-02-01");
        assert_eq!(r[0].valid_to.unwrap().to_string(), "2026-02-28");
    }

    #[test]
    fn hmrc_long_dates() {
        let text = "Country/territory,Currency,Currency code,Currency units per £1,Start date,End date\n\
Ukraine,Hryvnia,UAH,58.0164,1 Feb 2026,28 Feb 2026\n";
        let r = parse_hmrc_csv(text).unwrap();
        assert_eq!(r[0].date.to_string(), "2026-02-01");
    }

    #[test]
    fn hmrc_url() {
        assert!(hmrc_monthly_url(2026, 2).ends_with("monthly_csv_2026-2.csv"));
    }
}
