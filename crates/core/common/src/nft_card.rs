//! On-chain ERC-721 metadata: a base64 `data:` JSON document whose image is an
//! SVG card drawn in the same layout as the Intex card.
//!
//! Every string that reaches the document is a constant, a number or hex, so
//! nothing needs JSON or XML escaping.

use alloy_primitives::U256;
use base64::{engine::general_purpose::STANDARD, Engine};
use core::fmt::Write;

const SCALE_1E6: u64 = 1_000_000;
const SECONDS_PER_DAY: u64 = 86_400;

/// A lifecycle state as the card shows it: the attribute label and the badge
/// color shared with the Intex card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct State {
    pub label: &'static str,
    pub color: &'static str,
}

pub const ISSUED: State = State {
    label: "Issued",
    color: "#2563eb",
};
pub const OPEN: State = State {
    label: "Open",
    color: "#2563eb",
};
pub const QUALIFIED: State = State {
    label: "Qualified",
    color: "#16a34a",
};
pub const CALLED: State = State {
    label: "Called",
    color: "#f97316",
};
pub const EXPIRED: State = State {
    label: "Expired",
    color: "#6b7280",
};
pub const VOID: State = State {
    label: "Void",
    color: "#6b7280",
};
pub const SETTLED: State = State {
    label: "Settled",
    color: "#a855f7",
};

/// One `attributes` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trait {
    Text(&'static str, String),
    Number(&'static str, String),
    Date(&'static str, u64),
}

impl Trait {
    pub fn text(name: &'static str, value: impl Into<String>) -> Self {
        Self::Text(name, value.into())
    }

    pub fn integer(name: &'static str, value: impl Into<u64>) -> Self {
        Self::Number(name, value.into().to_string())
    }

    /// A six-decimal protocol amount, shown as a plain decimal number.
    pub fn amount(name: &'static str, minor: U256) -> Self {
        Self::Number(name, amount(minor))
    }

    pub fn date(name: &'static str, timestamp: u64) -> Self {
        Self::Date(name, timestamp)
    }

    fn write_json(&self, out: &mut String) {
        match self {
            Self::Text(name, value) => {
                let _ = write!(out, r#"{{"trait_type":"{name}","value":"{value}"}}"#);
            }
            Self::Number(name, value) => {
                let _ = write!(
                    out,
                    r#"{{"trait_type":"{name}","value":{value},"display_type":"number"}}"#
                );
            }
            Self::Date(name, value) => {
                let _ = write!(
                    out,
                    r#"{{"trait_type":"{name}","value":{value},"display_type":"date"}}"#
                );
            }
        }
    }
}

/// The picture: a title, the token's short id, a state badge and labelled rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card<'a> {
    pub title: &'a str,
    pub subtitle: &'a str,
    pub state: State,
    pub rows: Vec<(&'static str, String)>,
}

impl Card<'_> {
    pub fn svg(&self) -> String {
        let color = self.state.color;
        let mut out = String::with_capacity(2_048);
        out.push_str(r#"<svg width="600" height="600" xmlns="http://www.w3.org/2000/svg">"#);
        out.push_str(r##"<rect width="600" height="600" fill="#1a1a1a" rx="20"/>"##);
        out.push_str(
            r##"<rect x="15" y="15" width="570" height="570" fill="none" stroke="#444" stroke-width="2" rx="15"/>"##,
        );
        let _ = write!(
            out,
            r##"<text x="300" y="70" font-family="sans-serif" font-size="32" font-weight="bold" fill="#fff" text-anchor="middle">{}</text>"##,
            self.title
        );
        let _ = write!(
            out,
            r##"<text x="300" y="110" font-family="sans-serif" font-size="24" font-weight="600" fill="#cbd5f5" text-anchor="middle">{}</text>"##,
            self.subtitle
        );
        let _ = write!(
            out,
            r#"<rect x="200" y="130" width="200" height="50" fill="{color}" rx="25" opacity="0.3"/>"#
        );
        let _ = write!(
            out,
            r#"<text x="300" y="163" font-family="sans-serif" font-size="22" font-weight="bold" fill="{color}" text-anchor="middle">{}</text>"#,
            self.state.label.to_ascii_uppercase()
        );
        out.push_str(
            r##"<line x1="60" y1="220" x2="540" y2="220" stroke="#444" stroke-width="2"/>"##,
        );
        for (index, (label, value)) in self.rows.iter().enumerate() {
            let y = 265 + 45 * index;
            let _ = write!(
                out,
                r##"<text x="60" y="{y}" font-family="sans-serif" font-size="20" fill="#999">{label}</text><text x="540" y="{y}" font-family="sans-serif" font-size="20" fill="#fff" text-anchor="end">{value}</text>"##
            );
        }
        out.push_str("</svg>");
        out
    }
}

/// `data:application/json;base64,...` carrying the card as its `image`.
pub fn token_uri(name: &str, description: &str, card: &Card<'_>, traits: &[Trait]) -> String {
    let mut json = String::with_capacity(4_096);
    let _ = write!(
        json,
        r#"{{"name":"{name}","description":"{description}","image":"data:image/svg+xml;base64,{}","attributes":["#,
        STANDARD.encode(card.svg())
    );
    for (index, entry) in traits.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        entry.write_json(&mut json);
    }
    json.push_str("]}");
    format!("data:application/json;base64,{}", STANDARD.encode(json))
}

/// A six-decimal amount with trailing fraction zeros trimmed: `2.28`, `0.000001`.
pub fn amount(minor: U256) -> String {
    let (whole, fraction) = split(minor);
    format!("{whole}{fraction}")
}

/// [`amount`] with thousands separators, for the card: `100,000`, `1,234.5`.
pub fn amount_grouped(minor: U256) -> String {
    let (whole, fraction) = split(minor);
    let digits = whole.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + fraction.len());
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out.push_str(&fraction);
    out
}

fn split(minor: U256) -> (U256, String) {
    let scale = U256::from(SCALE_1E6);
    let remainder = (minor % scale).to::<u64>();
    let fraction = if remainder == 0 {
        String::new()
    } else {
        let digits = format!("{remainder:06}");
        format!(".{}", digits.trim_end_matches('0'))
    };
    (minor / scale, fraction)
}

/// `DD.MM.YYYY HH:MM UTC`, via Howard Hinnant's `civil_from_days`.
pub fn timestamp_utc(timestamp: u64) -> String {
    let z = timestamp / SECONDS_PER_DAY + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    let seconds = timestamp % SECONDS_PER_DAY;
    format!(
        "{day:02}.{month:02}.{year} {:02}:{:02} UTC",
        seconds / 3_600,
        seconds % 3_600 / 60
    )
}

/// `0x3fa2b1...9c0e`: enough of a 256-bit id to tell cards apart.
pub fn short_id(id: U256) -> String {
    let hex = format!("{id:064x}");
    format!("0x{}...{}", &hex[..6], &hex[60..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(uri: &str) -> String {
        let payload = uri
            .strip_prefix("data:application/json;base64,")
            .expect("json data uri");
        String::from_utf8(STANDARD.decode(payload).unwrap()).unwrap()
    }

    #[test]
    fn amounts_match_the_intex_card_formatting() {
        assert_eq!(amount(U256::from(12_000_000u64)), "12");
        assert_eq!(amount(U256::from(2_280_000u64)), "2.28");
        assert_eq!(amount(U256::from(1u64)), "0.000001");
        assert_eq!(amount(U256::from(1_234u64)), "0.001234");
        assert_eq!(amount(U256::ZERO), "0");
        assert_eq!(amount_grouped(U256::from(100_000_000_000u64)), "100,000");
        assert_eq!(amount_grouped(U256::from(1_234_500_000u64)), "1,234.5");
        assert_eq!(amount_grouped(U256::from(999_000_000u64)), "999");
    }

    #[test]
    fn timestamps_render_as_utc_calendar_dates() {
        assert_eq!(timestamp_utc(1_209_600), "15.01.1970 00:00 UTC");
        assert_eq!(timestamp_utc(1_709_164_800), "29.02.2024 00:00 UTC");
        assert_eq!(timestamp_utc(1_893_455_999), "31.12.2029 23:59 UTC");
    }

    #[test]
    fn short_id_keeps_both_ends() {
        let id = U256::from_be_bytes([0xab; 32]) ^ U256::from(0xffffu64);
        assert_eq!(short_id(id), "0xababab...5454");
    }

    #[test]
    fn token_uri_embeds_the_card_and_typed_attributes() {
        let card = Card {
            title: "GEM",
            subtitle: "0xababab...5454",
            state: QUALIFIED,
            rows: vec![("Call Price", amount_grouped(U256::from(2_280_000u64)))],
        };
        let json = decode(&token_uri(
            "Gem 0xababab...5454",
            "Outbe Gem",
            &card,
            &[
                Trait::text("State", "Qualified"),
                Trait::amount("Call Price", U256::from(2_280_000u64)),
                Trait::integer("Issuance Currency", 840u16),
                Trait::date("Issued At", 1_700_000_000),
            ],
        ));
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["name"], "Gem 0xababab...5454");
        assert_eq!(value["description"], "Outbe Gem");
        assert!(json.contains(r#"{"trait_type":"State","value":"Qualified"}"#));
        assert!(
            json.contains(r#"{"trait_type":"Call Price","value":2.28,"display_type":"number"}"#)
        );
        assert!(json
            .contains(r#"{"trait_type":"Issuance Currency","value":840,"display_type":"number"}"#));
        assert!(
            json.contains(r#"{"trait_type":"Issued At","value":1700000000,"display_type":"date"}"#)
        );

        let image = value["image"]
            .as_str()
            .unwrap()
            .strip_prefix("data:image/svg+xml;base64,")
            .unwrap();
        let svg = String::from_utf8(STANDARD.decode(image).unwrap()).unwrap();
        assert_eq!(svg, card.svg());
        assert!(svg.contains(">GEM</text>"));
        assert!(svg.contains(">QUALIFIED</text>"));
        assert!(svg.contains(r##"fill="#16a34a""##));
        assert!(svg.contains(">2.28</text>"));
    }
}
