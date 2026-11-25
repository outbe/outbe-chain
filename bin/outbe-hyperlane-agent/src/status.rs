//! The `status` command: reads `/status` of the running agent and prints it
//! as a table.

use std::fmt::Write;

use eyre::{Context, Result};
use serde_json::Value;

/// The URL of the health server of this host for a `[health] bind_address`.
pub fn local_url(bind_address: &str) -> String {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

    let address = match bind_address.parse::<SocketAddr>() {
        // A server on every address answers on the loopback address.
        Ok(bound) if bound.ip().is_unspecified() => {
            let loopback = match bound {
                SocketAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
                SocketAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
            };
            SocketAddr::new(loopback, bound.port()).to_string()
        }
        _ => bind_address.to_owned(),
    };
    format!("http://{address}/status")
}

pub async fn run(url: &str) -> Result<()> {
    let status: Value = reqwest::get(url)
        .await
        .and_then(|r| r.error_for_status())
        .with_context(|| format!("no status from {url}: is the agent running?"))?
        .json()
        .await?;
    print!("{}", render(&status));
    Ok(())
}

/// `-` for a value that the agent does not know yet.
fn cell(value: &Value) -> String {
    match value {
        Value::Null => "-".to_owned(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// A wei amount as native tokens with four decimals.
fn tokens(wei: &Value) -> String {
    let Some(wei) = wei
        .as_str()
        .filter(|w| w.bytes().all(|b| b.is_ascii_digit()))
    else {
        return "-".to_owned();
    };
    let padded = format!("{wei:0>19}");
    let (whole, fraction) = padded.split_at(padded.len() - 18);
    format!("{whole}.{}", &fraction[..4])
}

fn render(status: &Value) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "healthy: {}   controller: {}   active: {}   misses: {}",
        cell(&status["healthy"]),
        cell(&status["controller"]),
        cell(&status["active"]),
        cell(&status["miss_count"]),
    );
    let relayer = &status["relayer"];
    let relaying = relayer["enabled"] == true;
    let _ = writeln!(
        out,
        "relayer: {}   delivered: {}",
        if relaying { "on" } else { "off" },
        cell(&relayer["delivered"]),
    );
    for error in ["signer_error", "last_submit_error"] {
        if let Some(text) = status[error].as_str() {
            let _ = writeln!(out, "{error}: {text}");
        }
    }
    let _ = writeln!(
        out,
        "\n{:<12}{:>9}{:>9}{:>12}{:>8}{:>7}{:>9}{:>14}  state",
        "domain", "leaves", "signed", "head", "behind", "queue", "dropped", "balance"
    );
    let empty = serde_json::Map::new();
    for (domain, d) in status["domains"].as_object().unwrap_or(&empty) {
        let state = match (
            d["error"].as_str(),
            d["reorg"] == true,
            d["behind"].as_u64(),
        ) {
            (Some(error), ..) => error.to_owned(),
            _ if d["relay"]["error"].is_string() => cell(&d["relay"]["error"]),
            (_, true, _) => "halted: reorg".to_owned(),
            (_, _, Some(behind)) if behind > 0 => "syncing".to_owned(),
            (_, _, None) => "starting".to_owned(),
            _ => "ok".to_owned(),
        };
        let _ = writeln!(
            out,
            "{:<12}{:>9}{:>9}{:>12}{:>8}{:>7}{:>9}{:>14}  {state}",
            domain,
            cell(&d["leaves"]),
            cell(&d["latest_signed_index"]),
            cell(&d["head"]),
            cell(&d["behind"]),
            cell(&d["relay"]["queue"]),
            cell(&d["relay"]["dropped"]),
            tokens(&d["balance_wei"]),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn table_shows_each_domain_state() {
        let status = json!({
            "healthy": false, "controller": "ready", "active": true, "miss_count": 0,
            "signer_error": null, "last_submit_error": "nonce too low",
            "relayer": {"enabled": true, "delivered": 12},
            "domains": {
                "97": {"leaves": 388, "latest_signed_index": 387, "head": 5000, "behind": 0,
                       "reorg": false, "error": null, "relay": {"queue": 1, "dropped": 0},
                       "balance_wei": "1500000000000000000"},
                "5": {"leaves": 90, "latest_signed_index": 80, "head": 700, "behind": 14,
                      "reorg": false, "error": null, "relay": {"queue": 0, "dropped": 2},
                      "balance_wei": "20000000000000"},
                "6": {"leaves": 0, "latest_signed_index": null, "head": null, "behind": null,
                      "reorg": false, "error": "domain 6 has no [[origin]] block"},
            },
        });
        let text = render(&status);
        let line = |domain: &str| {
            let line = text.lines().find(|l| l.starts_with(domain)).unwrap();
            line.split_whitespace()
                .map(String::from)
                .collect::<Vec<_>>()
        };
        assert!(text.contains("relayer: on   delivered: 12"), "{text}");
        assert!(text.contains("last_submit_error: nonce too low"));
        assert_eq!(
            line("97"),
            ["97", "388", "387", "5000", "0", "1", "0", "1.5000", "ok"]
        );
        assert_eq!(line("5 ")[4..], ["14", "0", "2", "0.0000", "syncing"]);
        assert_eq!(line("6 ")[1..8], ["0", "-", "-", "-", "-", "-", "-"]);
        assert!(text.contains("no [[origin]] block"));
        assert_eq!(local_url("0.0.0.0:9003"), "http://127.0.0.1:9003/status");
        assert_eq!(local_url("10.0.0.0:9003"), "http://10.0.0.0:9003/status");
        assert_eq!(local_url("[::]:9003"), "http://[::1]:9003/status");
    }
}
