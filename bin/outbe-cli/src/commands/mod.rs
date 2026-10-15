pub mod chain;
pub mod epoch;
pub mod monitor;
pub mod oracle;
pub mod rad;
pub mod radicle;
pub mod rewards;
pub mod slash;
pub mod stablecoin;
pub mod staking;
pub mod tee;
pub mod tribute;
pub mod validator;
pub mod vote;
pub mod zerofee;

use crate::errors::CommandInputError;
use crate::tx::TxSigner;
use alloy_primitives::U256;
use eyre::Result;

/// Parse a non-zero amount in decimal base units.
pub fn parse_amount(amount: &str) -> Result<U256> {
    let amount = U256::from_str_radix(amount, 10).map_err(CommandInputError::InvalidAmount)?;
    if amount.is_zero() {
        return Err(CommandInputError::ZeroAmount.into());
    }
    Ok(amount)
}

pub fn require_signer(private_key: Option<&str>) -> Result<TxSigner> {
    let key = private_key.ok_or(CommandInputError::PrivateKeyRequired)?;
    TxSigner::new(key)
}

fn format_decimals(value: alloy_primitives::U256, decimals: u8) -> String {
    use alloy_primitives::U256;

    if value.is_zero() {
        return "0".to_string();
    }

    let divisor = U256::from(10u64).pow(U256::from(decimals));
    let whole = value / divisor;
    let frac = value % divisor;

    if frac.is_zero() {
        format!("{whole}")
    } else {
        let frac_str = format!("{frac:0>width$}", width = usize::from(decimals));
        let trimmed = frac_str.trim_end_matches('0');
        format!("{whole}.{trimmed}")
    }
}

/// Format a raw native COEN amount in 18-decimal base units.
pub fn format_coen_amount(value: alloy_primitives::U256) -> String {
    format_decimals(value, 18)
}

/// Format a six-decimal protocol amount, such as a COEN/ISO oracle rate.
pub fn format_protocol_amount(value: alloy_primitives::U256) -> String {
    format_decimals(value, 6)
}

/// Format an independently owned FP18 value.
pub fn format_generic_fp18(value: alloy_primitives::U256) -> String {
    format_decimals(value, 18)
}

/// Extracts the last 40 hex chars from a topic (address is zero-padded to 32 bytes).
pub(crate) fn format_topic_addr(topic: &str) -> String {
    let s = topic.strip_prefix("0x").unwrap_or(topic);
    if s.len() >= 40 {
        format!("0x{}", &s[s.len() - 40..])
    } else {
        topic.to_string()
    }
}

pub(crate) struct ParsedLog<'a> {
    pub(crate) block: u64,
    pub(crate) topics: Vec<&'a str>,
    pub(crate) bytes: Vec<u8>,
}

impl<'a> ParsedLog<'a> {
    pub(crate) fn from_value(log: &'a serde_json::Value) -> Self {
        let block = log["blockNumber"]
            .as_str()
            .and_then(|s| u64::from_str_radix(s.strip_prefix("0x").unwrap_or(s), 16).ok())
            .unwrap_or(0);
        let topics = log["topics"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        let data = log["data"].as_str().unwrap_or("0x");
        let data_hex = data.strip_prefix("0x").unwrap_or(data);
        let bytes = hex::decode(data_hex).unwrap_or_default();
        Self {
            block,
            topics,
            bytes,
        }
    }

    pub(crate) fn topic(&self, index: usize) -> &'a str {
        self.topics.get(index).copied().unwrap_or("")
    }

    pub(crate) fn topic_addr(&self, index: usize) -> String {
        format_topic_addr(self.topics.get(index).copied().unwrap_or("?"))
    }

    pub(crate) fn word_u256(&self, word_index: usize) -> U256 {
        let start = word_index * 32;
        let end = start + 32;
        if self.bytes.len() >= end {
            U256::from_be_slice(&self.bytes[start..end])
        } else {
            U256::ZERO
        }
    }

    pub(crate) fn word_u64(&self, word_index: usize) -> u64 {
        self.word_u256(word_index).to::<u64>()
    }
}

pub(crate) fn recent_logs(logs: &[serde_json::Value], limit: usize) -> &[serde_json::Value] {
    let start = logs.len().saturating_sub(limit);
    &logs[start..]
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::U256;

    #[test]
    fn test_format_unit_zero() {
        assert_eq!(format_coen_amount(U256::ZERO), "0");
    }

    #[test]
    fn test_format_unit_one_coen() {
        assert_eq!(
            format_coen_amount(U256::from(1_000_000_000_000_000_000u128)),
            "1"
        );
    }

    #[test]
    fn test_format_unit_large_whole() {
        let val = U256::from(1_000_000_000_000_000_000_000u128);
        assert_eq!(format_coen_amount(val), "1000");
    }

    #[test]
    fn test_format_unit_fractional() {
        let val = U256::from(1_500_000_000_000_000_000u128);
        assert_eq!(format_coen_amount(val), "1.5");
    }

    #[test]
    fn test_format_unit_pure_fraction() {
        let val = U256::from(500_000_000_000_000_000u128);
        assert_eq!(format_coen_amount(val), "0.5");
    }

    #[test]
    fn test_format_unit_one_native_unit() {
        assert_eq!(format_coen_amount(U256::from(1u64)), "0.000000000000000001");
    }

    #[test]
    fn test_format_unit_trailing_zeros_trimmed() {
        let val = U256::from(1_200_000_000_000_000_000u128);
        assert_eq!(format_coen_amount(val), "1.2");
    }

    #[test]
    fn test_format_unit_all_decimal_places() {
        let val = U256::from(999_999_999_999_999_999u128);
        assert_eq!(format_coen_amount(val), "0.999999999999999999");
    }

    #[test]
    fn test_format_protocol_amount_remains_six_decimals() {
        assert_eq!(format_protocol_amount(U256::from(1_500_000u64)), "1.5");
    }

    #[test]
    fn test_format_unit_max_u256_never_panics() {
        let result = format_coen_amount(U256::MAX);
        assert!(!result.is_empty());
        assert!(result.contains('.'));
    }
}
