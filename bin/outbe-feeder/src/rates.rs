//! Policy rate votes: the overnight reference rate of each currency.
//!
//! For each `[[rate_providers]]` entry the feeder reads the rate and its date
//! from the source of that currency. A source publishes one rate for each
//! business day, so the feeder asks a source again only after
//! [`REFRESH_SECS`] and votes the last value it read in between.
//!
//! A vote carries the annual rate at scale 1e6 and the publication time: the
//! date that the source gives with the rate, at 00:00 UTC. A rate below zero
//! counts as zero.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use alloy_primitives::U256;
use eyre::{ensure, eyre, Result};
use outbe_system_common::time::{date_key_to_utc_timestamp, timestamp_to_date_key};
use serde::Deserialize;
use serde_json::Value;
use tracing::warn;

/// The feeder asks a source again after this time.
const REFRESH_SECS: u64 = 10 * 60;
/// Time limit of all rate reads of one vote.
const COLLECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Time limit of one read of a source.
const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// The Bank of England and the Bank of Japan need a start date. The feeder
/// asks for the values of this many days.
const HISTORY_DAYS: u64 = 14;
/// One percent at scale 1e6.
const PERCENT_1E6: u64 = 10_000;
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// One currency to vote a policy rate for.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateProviderConfig {
    /// ISO 4217 numeric code of the currency, as the quote of a pair.
    pub currency: String,
    /// The form of the answer of the source.
    pub kind: RateKind,
    /// Address of the source. Not used by `fixed`.
    #[serde(default)]
    pub url: String,
    /// The annual rate in percent, for `fixed` only.
    #[serde(default)]
    pub rate: String,
}

/// The form of the answer of a rate source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RateKind {
    /// Federal Reserve Bank of New York, JSON.
    Nyfed,
    /// European Central Bank, SDMX JSON.
    Ecb,
    /// Bank of England, CSV.
    Boe,
    /// Bank of Japan, JSON.
    Boj,
    /// Hong Kong Monetary Authority, JSON.
    Hkma,
    /// China Foreign Exchange Trade System, JSON.
    Cfets,
    /// The `rate` of the entry, with the current UTC day as the date. For a
    /// network that is not Mainnet.
    Fixed,
}

/// A rate vote: `(currency, annual rate at scale 1e6, publication time)`.
pub type RateVote = (u16, U256, u64);

/// The last rate read from each source.
#[derive(Default)]
pub struct RateCache {
    http: reqwest::Client,
    rates: HashMap<u16, (U256, u64)>,
    /// When the feeder last asked each source, also when the read failed.
    asked: HashMap<u16, Instant>,
}

impl RateCache {
    /// The rate votes of this vote, within [`COLLECT_TIMEOUT`]. A timeout
    /// gives no rates, and the pair prices still go.
    /// Reads the sources that are due, within [`COLLECT_TIMEOUT`]. A source
    /// that fails or is slow keeps its last value.
    pub async fn refresh(&mut self, providers: &[RateProviderConfig]) {
        if providers.iter().all(|entry| entry.kind == RateKind::Fixed) {
            return;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        if tokio::time::timeout(COLLECT_TIMEOUT, self.read_due(providers, now))
            .await
            .is_err()
        {
            warn!("policy rates took longer than 15 seconds; keeping the last rates");
        }
    }

    /// The rate votes for a vote on top of a block with time `block_time`.
    ///
    /// A fixed rate has the UTC day of the block time. A currency with no
    /// value gets no vote, and the Oracle keeps its last rate. A rate with a
    /// publication time after the block time gets no vote: the Oracle rejects
    /// the whole vote for such a rate.
    pub fn votes(&self, providers: &[RateProviderConfig], block_time: u64) -> Vec<RateVote> {
        entries(providers)
            .into_iter()
            .filter_map(|(currency, entry)| {
                let (rate, published_at) = if entry.kind == RateKind::Fixed {
                    fixed(entry, block_time)
                        .inspect_err(|error| warn!(currency, %error, "invalid fixed rate"))
                        .ok()?
                } else {
                    *self.rates.get(&currency)?
                };
                (published_at <= block_time).then_some((currency, rate, published_at))
            })
            .collect()
    }

    /// Reads each source that the feeder did not ask in the last
    /// [`REFRESH_SECS`]. `now` is the Unix time of the host.
    async fn read_due(&mut self, providers: &[RateProviderConfig], now: u64) {
        let due: Vec<_> = entries(providers)
            .into_iter()
            .filter(|(currency, entry)| {
                entry.kind != RateKind::Fixed
                    && self
                        .asked
                        .get(currency)
                        .is_none_or(|asked| asked.elapsed().as_secs() >= REFRESH_SECS)
            })
            .collect();
        // The sources are read in parallel: the time is that of the slowest.
        let http = &self.http;
        let reads = due
            .iter()
            .map(|(currency, entry)| async move { (*currency, read(http, entry, now).await) });
        for (currency, result) in futures::future::join_all(reads).await {
            self.asked.insert(currency, Instant::now());
            match result {
                Ok(rate) => {
                    self.rates.insert(currency, rate);
                }
                Err(error) => warn!(currency, %error, "rate source failed; keeping the last rate"),
            }
        }
    }
}

/// The providers with a valid currency code.
fn entries(providers: &[RateProviderConfig]) -> Vec<(u16, &RateProviderConfig)> {
    providers
        .iter()
        .filter_map(|entry| match entry.currency.parse::<u16>() {
            Ok(currency) => Some((currency, entry)),
            Err(_) => {
                warn!(currency = %entry.currency, "invalid rate currency; no rate vote");
                None
            }
        })
        .collect()
}

/// The fixed rate of `entry`, published at 00:00 UTC of the day of
/// `block_time`.
fn fixed(entry: &RateProviderConfig, block_time: u64) -> Result<(U256, u64)> {
    let day = date_key_to_utc_timestamp(timestamp_to_date_key(block_time))
        .map_err(|_| eyre!("invalid time {block_time}"))?;
    Ok((percent_to_1e6(&entry.rate)?, day))
}

/// Reads the rate of `entry` and its publication time from its source.
async fn read(http: &reqwest::Client, entry: &RateProviderConfig, now: u64) -> Result<(U256, u64)> {
    let from = timestamp_to_date_key(now.saturating_sub(HISTORY_DAYS * 86_400));
    let (year, month, day) = (from / 10_000, from / 100 % 100, from % 100);
    let url = match entry.kind {
        RateKind::Boe => format!(
            "{}&Datefrom={day:02}/{}/{year}&Dateto=now",
            entry.url,
            MONTHS[(month as usize).saturating_sub(1) % 12]
        ),
        RateKind::Boj => format!("{}&startDate={year}{month:02}", entry.url),
        _ => entry.url.clone(),
    };
    let body = http
        .get(url)
        .header(reqwest::header::USER_AGENT, "outbe-feeder")
        .timeout(READ_TIMEOUT)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    parse(entry.kind, &body)
}

/// The rate at scale 1e6 and its publication time from the answer `body` of
/// a source of `kind`.
fn parse(kind: RateKind, body: &str) -> Result<(U256, u64)> {
    let json = || serde_json::from_str::<Value>(body).map_err(|error| eyre!("not JSON: {error}"));
    let (rate, date) = match kind {
        RateKind::Nyfed => {
            let json = json()?;
            let row = &json["refRates"][0];
            (
                text(&row["percentRate"])?,
                iso_date(&text(&row["effectiveDate"])?)?,
            )
        }
        RateKind::Ecb => {
            let json = json()?;
            let rate = &json["dataSets"][0]["series"]["0:0:0"]["observations"]["0"][0];
            let date = &json["structure"]["dimensions"]["observation"][0]["values"][0]["id"];
            (text(rate)?, iso_date(&text(date)?)?)
        }
        RateKind::Boe => {
            // Rows as `13 Nov 2026,3.9832`. The last row is the newest.
            let row = body
                .lines()
                .map(str::trim)
                .rfind(|line| !line.is_empty())
                .ok_or_else(|| eyre!("no rows"))?;
            let (date, rate) = row
                .split_once(',')
                .ok_or_else(|| eyre!("bad row {row:?}"))?;
            let mut parts = date.split_whitespace();
            let mut part = || parts.next().ok_or_else(|| eyre!("bad date {date:?}"));
            let (day, month, year) = (part()?, part()?, part()?);
            let month = MONTHS
                .iter()
                .position(|name| name.eq_ignore_ascii_case(month))
                .ok_or_else(|| eyre!("bad month in {date:?}"))?;
            (
                rate.to_owned(),
                day_start(year.parse()?, month as u32 + 1, day.parse()?)?,
            )
        }
        RateKind::Boj => {
            // Two arrays of equal length. A day with no rate has `null`.
            let json = json()?;
            let values = &json["RESULTSET"][0]["VALUES"];
            let rates = values["VALUES"]
                .as_array()
                .ok_or_else(|| eyre!("no VALUES"))?;
            let dates = values["SURVEY_DATES"]
                .as_array()
                .ok_or_else(|| eyre!("no SURVEY_DATES"))?;
            let index = rates
                .iter()
                .rposition(|rate| !rate.is_null())
                .ok_or_else(|| eyre!("no rate"))?;
            let date: u32 = text(dates.get(index).unwrap_or(&Value::Null))?.parse()?;
            (
                text(&rates[index])?,
                day_start(date / 10_000, date / 100 % 100, date % 100)?,
            )
        }
        RateKind::Hkma => {
            let json = json()?;
            let row = &json["result"]["records"][0];
            (
                text(&row["hibor_overnight"])?,
                iso_date(&text(&row["end_of_date"])?)?,
            )
        }
        RateKind::Cfets => {
            // The date is `26-11-17`: two digits for the year.
            let json = json()?;
            let row = json["records"]
                .as_array()
                .and_then(|rows| rows.iter().find(|row| row["productCode"] == "DR001"))
                .ok_or_else(|| eyre!("no DR001 row"))?;
            (
                text(&row["weightedRate"])?,
                iso_date(&format!("20{}", text(&row["date"])?))?,
            )
        }
        RateKind::Fixed => return Err(eyre!("a fixed rate has no source")),
    };
    Ok((percent_to_1e6(&rate)?, date))
}

/// A JSON string or number as text. A number keeps the digits of the source.
fn text(value: &Value) -> Result<String> {
    match value {
        Value::String(text) => Ok(text.clone()),
        Value::Number(number) => Ok(number.to_string()),
        other => Err(eyre!("expected a string or a number, found {other}")),
    }
}

/// 00:00 UTC of the date `YYYY-MM-DD`, in Unix seconds.
fn iso_date(date: &str) -> Result<u64> {
    let mut parts = date.split('-').map(str::parse::<u32>);
    let mut part = || parts.next().ok_or_else(|| eyre!("bad date {date:?}"));
    day_start(part()??, part()??, part()??)
}

/// 00:00 UTC of a calendar date, in Unix seconds.
fn day_start(year: u32, month: u32, day: u32) -> Result<u64> {
    ensure!(
        (1970..=9999).contains(&year) && (1..=12).contains(&month) && (1..=31).contains(&day),
        "bad date {year}-{month}-{day}"
    );
    date_key_to_utc_timestamp(year * 10_000 + month * 100 + day)
        .map_err(|_| eyre!("bad date {year}-{month}-{day}"))
}

/// A rate in percent, for example `3.9832`, as an annual rate at scale 1e6.
/// The digits after the fourth decimal are cut. A rate below zero is zero.
fn percent_to_1e6(percent: &str) -> Result<U256> {
    let percent = percent.trim();
    if percent.starts_with('-') {
        return Ok(U256::ZERO);
    }
    let (whole, fraction) = percent.split_once('.').unwrap_or((percent, ""));
    ensure!(
        !whole.is_empty() && fraction.bytes().all(|digit| digit.is_ascii_digit()),
        "bad rate {percent:?}"
    );
    let whole: u64 = whole.parse().map_err(|_| eyre!("bad rate {percent:?}"))?;
    let fraction: u64 = format!("{:0<4.4}", fraction).parse()?;
    whole
        .checked_mul(PERCENT_1E6)
        .and_then(|rate| rate.checked_add(fraction))
        .map(U256::from)
        .ok_or_else(|| eyre!("rate {percent:?} is too large"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOV_13: u64 = 1_794_528_000;
    const NOV_16: u64 = NOV_13 + 3 * 86_400;
    const NOV_17: u64 = NOV_13 + 4 * 86_400;

    fn rate(value: u64) -> U256 {
        U256::from(value)
    }

    #[test]
    fn each_source_gives_its_rate_and_date() {
        for (kind, body, expected) in [
            (
                RateKind::Nyfed,
                r#"{ "refRates": [ { "effectiveDate": "2026-11-16", "type": "SOFR" ,"percentRate": 3.93 ,"volumeInBillions": 3170 } ] }"#,
                (rate(39_300), NOV_16),
            ),
            (
                RateKind::Ecb,
                r#"{"dataSets":[{"series":{"0:0:0":{"observations":{"0":[2.44,0,0,null]}}}}],"structure":{"dimensions":{"observation":[{"id":"TIME_PERIOD","values":[{"id":"2026-11-16","name":"2026-11-16"}]}]}}}"#,
                (rate(24_400), NOV_16),
            ),
            (
                RateKind::Boe,
                "DATE,IUDSOIA\r\n12 Nov 2026,3.9846\r\n13 Nov 2026,3.9832\r\n",
                (rate(39_832), NOV_13),
            ),
            (
                RateKind::Boj,
                r#"{"STATUS":200,"RESULTSET":[{"SERIES_CODE":"STRDCLUCON","VALUES":{"SURVEY_DATES":[20261112,20261113,20261114],"VALUES":[1.227,1.229,null]}}]}"#,
                (rate(12_290), NOV_13),
            ),
            (
                RateKind::Hkma,
                r#"{"header":{"success":true},"result":{"datasize":1,"records":[{"end_of_date":"2026-11-17","disc_win_base_rate":4.25,"hibor_overnight":2.25}]}}"#,
                (rate(22_500), NOV_17),
            ),
            (
                RateKind::Cfets,
                r#"{"data":{"showDateEN":"17/11/2026 17:00"},"records":[{"date":"26-11-17","productCode":"DR007","weightedRate":"1.4200"},{"date":"26-11-17","productCode":"DR001","latestRate":"1.2600","weightedRate":"1.3035"}]}"#,
                (rate(13_035), NOV_17),
            ),
        ] {
            assert_eq!(parse(kind, body).unwrap(), expected, "{kind:?}");
        }
    }

    #[test]
    fn a_bad_answer_gives_no_rate() {
        assert!(parse(RateKind::Nyfed, "<html>").is_err());
        assert!(parse(RateKind::Nyfed, r#"{"refRates":[]}"#).is_err());
        assert!(parse(RateKind::Boe, "").is_err());
        assert!(parse(
            RateKind::Boj,
            r#"{"RESULTSET":[{"VALUES":{"SURVEY_DATES":[20261114],"VALUES":[null]}}]}"#
        )
        .is_err());
        assert!(parse(RateKind::Cfets, r#"{"records":[]}"#).is_err());
    }

    #[test]
    fn zero_rate_is_a_rate() {
        assert_eq!(percent_to_1e6("0").unwrap(), U256::ZERO);
        assert_eq!(percent_to_1e6("-0.25").unwrap(), U256::ZERO);
        assert_eq!(percent_to_1e6("0.5").unwrap(), rate(5_000));
        assert_eq!(percent_to_1e6("4.31256").unwrap(), rate(43_125));
        assert!(percent_to_1e6("abc").is_err());
        assert!(percent_to_1e6("").is_err());
    }

    #[test]
    fn a_fixed_rate_has_the_day_of_the_block_and_a_future_rate_gets_no_vote() {
        let entry = |currency: &str, rate: &str| RateProviderConfig {
            currency: currency.to_owned(),
            kind: RateKind::Fixed,
            url: String::new(),
            rate: rate.to_owned(),
        };
        let mut cache = RateCache::default();

        let votes = cache.votes(&[entry("840", "4.3"), entry("x", "1")], NOV_17 + 3_600);
        assert_eq!(votes, vec![(840, rate(43_000), NOV_17)]);

        // The host is in the next day, and the block is not: the fixed rate
        // has the day of the block.
        let votes = cache.votes(&[entry("840", "4.3")], NOV_17 - 1);
        assert_eq!(votes, vec![(840, rate(43_000), NOV_16)]);

        // A source rate with a date after the block time gets no vote.
        let source = RateProviderConfig {
            kind: RateKind::Nyfed,
            ..entry("840", "")
        };
        cache.rates.insert(840, (rate(39_300), NOV_17));
        assert!(cache
            .votes(std::slice::from_ref(&source), NOV_17 - 1)
            .is_empty());
        assert_eq!(
            cache.votes(&[source], NOV_17),
            vec![(840, rate(39_300), NOV_17)]
        );
    }
}
