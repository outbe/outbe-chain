//! Bootstrap glue (ported `bootstrap-testnet.sh`): keys/DKG + genesis. The heavy
//! lifting stays one-shot subprocesses (`outbe-chain dkg bootstrap` and
//! `python3 seed_genesis.py`); the genesis skeleton, port rewrite, and dev felony
//! patch are native Rust.

use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use eyre::{bail, eyre, Result, WrapErr};
use serde_json::json;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use super::{worldwide_day, Localnet};

/// 10000 COEN (`10000 * 10^18`) as hex — the per-validator liquid balance.
const VALIDATOR_BALANCE_HEX: &str = "0x21E19E0C9BAB2400000";
/// Dev felony threshold (blocks) so downtime slashing is observable on the short
/// localnet epoch; must stay `<` the epoch length (`bootstrap-testnet.sh:234`).
const DEV_FELONY_THRESHOLD: u64 = 30;
const PROPOSER_FELONY_SLOT: u64 = 1;
const VOTER_FELONY_SLOT: u64 = 12;
/// A lifecycle E2E may opt into a short delay; production seed defaults remain
/// untouched. The value is supplied through `TESTNET_UNBONDING_PERIOD_SECS`.
const STAKING_SUFFIX: &str = "ee02";

impl Localnet {
    /// Keep a debug-only logical-clock E2E internally consistent by shifting the
    /// genesis header by the same signed number of seconds passed to every node.
    /// Without this, block 1 is correctly rejected by the production max-drift
    /// validator before a day-boundary scenario can exercise ZeroFee.
    pub(crate) fn shift_genesis_timestamp(&self, offset_secs: i64) -> Result<()> {
        let path = self.cfg.dir.join("genesis.json");
        let bytes = fs::read(&path)?;
        let mut genesis: serde_json::Value = serde_json::from_slice(&bytes)?;
        let raw = genesis
            .get("timestamp")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| eyre::eyre!("genesis timestamp is not a string"))?;
        let seconds = u64::from_str_radix(raw.trim_start_matches("0x"), 16)?;
        let shifted = i128::from(seconds) + i128::from(offset_secs);
        let shifted = u64::try_from(shifted)
            .map_err(|_| eyre::eyre!("genesis timestamp offset leaves u64 range"))?;
        genesis["timestamp"] = serde_json::Value::String(format!("0x{shifted:x}"));
        fs::write(path, serde_json::to_vec_pretty(&genesis)?)?;
        Ok(())
    }

    /// Bootstrap an N-validator set (keys, DKG, genesis). Runs unprivileged.
    /// `outbe-chain dkg bootstrap` and `seed_genesis.py` stay one-shot
    /// subprocesses; the genesis skeleton, port rewrite, and felony patch are
    /// native. `tuning` forwards `TESTNET_*` knobs (epoch length, DKG grace) some
    /// flows override; pass `&[]` for the defaults.
    pub fn bootstrap(&self, n: usize, tuning: &[(&str, String)]) -> Result<()> {
        fs::create_dir_all(&self.cfg.dir)?;

        // Step 1: DKG bootstrap — keys, polynomial, dkg-output, validators.json,
        // reth-bootnodes.txt.
        let mut cmd = Command::new(&self.cfg.bin_chain);
        cmd.args([
            "dkg",
            "bootstrap",
            "--output-dir",
            &self.dir(),
            "--validators",
            &n.to_string(),
        ]);
        for (k, v) in tuning {
            cmd.env(k, v);
        }
        self.run_setup(&mut cmd, "outbe-chain dkg bootstrap")?;

        // Step 1b: point the baked consensus/reth p2p endpoints at the resolved
        // ports (a no-op under the default layout).
        self.rewrite_ports()?;

        // Step 2: genesis skeleton (chain config + validator balances).
        self.write_genesis(tuning)?;

        // Step 2b: seed precompile storage (validator set/staking/etc.).
        self.seed_genesis(&worldwide_day())?;

        // Step 2c: dev felony thresholds for observable localnet slashing.
        self.patch_felony(tuning)?;
        // Step 2d: opt-in lifecycle timing for claim/accounting E2E scenarios.
        self.patch_staking_timing(tuning)?;
        Ok(())
    }

    /// Rewrite `validators.json` `p2p_address` to the resolved consensus port and
    /// each `reth-bootnodes.txt` enode's port to the resolved p2p port
    /// (`bootstrap-testnet.sh:105-128`).
    fn rewrite_ports(&self) -> Result<()> {
        let vpath = self.cfg.dir.join("validators.json");
        let mut v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&vpath)?)?;
        let arr = v
            .as_array_mut()
            .ok_or_else(|| eyre!("validators.json is not an array"))?;
        for (i, e) in arr.iter_mut().enumerate() {
            if let Some(obj) = e.as_object_mut() {
                obj.insert(
                    "p2p_address".into(),
                    json!(format!("127.0.0.1:{}", self.cfg.consensus_port(i))),
                );
            }
        }
        fs::write(&vpath, serde_json::to_string_pretty(&v)? + "\n")?;

        let bpath = self.cfg.dir.join("reth-bootnodes.txt");
        if let Ok(raw) = fs::read_to_string(&bpath) {
            let mut out = String::new();
            for (i, line) in raw.lines().filter(|l| !l.trim().is_empty()).enumerate() {
                // enode://<id>@<host>:<port> — swap the trailing port for p2p[i].
                match line.trim().rsplit_once(':') {
                    Some((head, _)) => {
                        out.push_str(head);
                        out.push_str(&format!(":{}\n", self.cfg.p2p_port(i)));
                    }
                    None => {
                        out.push_str(line.trim());
                        out.push('\n');
                    }
                }
            }
            fs::write(&bpath, out)?;
        }
        Ok(())
    }

    /// Write the genesis skeleton: static chain config (chain id 54322345, epoch /
    /// DKG params from `tuning`) plus a pre-funded `alloc` of each validator
    /// address (`bootstrap-testnet.sh:133-203`).
    fn write_genesis(&self, tuning: &[(&str, String)]) -> Result<()> {
        let epoch = tuned(tuning, "TESTNET_EPOCH_LENGTH_BLOCKS", 120);
        let dkg_prepare = tuned(tuning, "TESTNET_DKG_PREPARE_WINDOW_BLOCKS", 30);
        let dkg_grace = tuned(tuning, "TESTNET_DKG_ACTIVATION_GRACE_BLOCKS", 30);

        let vjson: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(self.cfg.dir.join("validators.json"))?)?;
        let arr = vjson
            .as_array()
            .ok_or_else(|| eyre!("validators.json is not an array"))?;
        let mut alloc = serde_json::Map::new();
        for e in arr {
            let addr = e
                .get("address")
                .and_then(|a| a.as_str())
                .ok_or_else(|| eyre!("validator entry missing address"))?;
            // Genesis alloc keys are the address without the `0x` prefix.
            let key = addr.trim_start_matches("0x").to_string();
            alloc.insert(key, json!({ "balance": VALIDATOR_BALANCE_HEX }));
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // genesisTime is one day in the past so the chain can produce immediately.
        let genesis_time = OffsetDateTime::from_unix_timestamp(now as i64 - 86_400)
            .wrap_err("genesis time")?
            .format(&Rfc3339)
            .wrap_err("format genesis time")?;

        let genesis = json!({
            "config": {
                "chainId": 54_322_345,
                "homesteadBlock": 0,
                "eip150Block": 0,
                "eip155Block": 0,
                "eip158Block": 0,
                "byzantiumBlock": 0,
                "constantinopleBlock": 0,
                "petersburgBlock": 0,
                "istanbulBlock": 0,
                "berlinBlock": 0,
                "londonBlock": 0,
                "mergeNetsplitBlock": 0,
                "terminalTotalDifficulty": 0,
                "terminalTotalDifficultyPassed": true,
                "shanghaiTime": 0,
                "cancunTime": 0,
                "pragueTime": 0,
                "epochLengthBlocks": epoch,
                "dkgPrepareWindowBlocks": dkg_prepare,
                "dkgActivationGraceBlocks": dkg_grace,
                "genesisTime": genesis_time,
            },
            "nonce": "0x0",
            "timestamp": format!("0x{now:x}"),
            "extraData": "0x",
            "gasLimit": "0x1c9c380",
            "difficulty": "0x0",
            "mixHash": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "coinbase": "0x0000000000000000000000000000000000000000",
            "alloc": alloc,
        });
        fs::write(
            self.cfg.dir.join("genesis.json"),
            serde_json::to_string_pretty(&genesis)? + "\n",
        )?;
        Ok(())
    }

    /// Seed precompile storage into genesis (`bootstrap-testnet.sh:209-226`) via
    /// the kept `scripts/seed_genesis.py`.
    fn seed_genesis(&self, worldwide_day: &str) -> Result<()> {
        let genesis = self.cfg.dir.join("genesis.json");
        let mut cmd = Command::new("python3");
        cmd.arg(self.cfg.repo.join("scripts/seed_genesis.py"))
            .arg("--genesis")
            .arg(&genesis)
            .arg("--seed")
            .arg(&self.cfg.seed)
            .arg("--validators")
            .arg(self.cfg.dir.join("validators.json"))
            .arg("--worldwide-day")
            .arg(worldwide_day)
            .arg("--output")
            .arg(&genesis);
        self.run_setup(&mut cmd, "seed_genesis.py")
    }

    /// Lower the SlashIndicator felony thresholds so downtime slashing triggers
    /// within the short dev epoch (`bootstrap-testnet.sh:228-253`).
    fn patch_felony(&self, tuning: &[(&str, String)]) -> Result<()> {
        let epoch = tuned(tuning, "TESTNET_EPOCH_LENGTH_BLOCKS", 120);
        if DEV_FELONY_THRESHOLD >= epoch {
            bail!("dev felony threshold {DEV_FELONY_THRESHOLD} must be < epoch length {epoch}");
        }
        let path = self.cfg.dir.join("genesis.json");
        let mut g: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path)?)?;
        let alloc = g
            .get_mut("alloc")
            .and_then(|a| a.as_object_mut())
            .ok_or_else(|| eyre!("genesis has no alloc object"))?;

        // SlashIndicator lives at 0x…ee01 (config slot 1 = proposer felony, slot
        // 12 = voter felony). Match the address however it's spelled in alloc.
        let key = alloc.keys().find(|k| ends_with_ee01(k)).cloned();
        let key = key.unwrap_or_else(|| {
            let k = "0x000000000000000000000000000000000000ee01".to_string();
            alloc.insert(k.clone(), json!({ "balance": "0x0", "code": "0xef0000" }));
            k
        });
        let entry = alloc
            .get_mut(&key)
            .and_then(|e| e.as_object_mut())
            .ok_or_else(|| eyre!("felony alloc entry is not an object"))?;
        let storage = entry
            .entry("storage")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(|| eyre!("felony storage is not an object"))?;
        patch_felony_storage(storage);

        fs::write(&path, serde_json::to_string_pretty(&g)? + "\n")?;
        Ok(())
    }

    /// Apply opt-in staking lifecycle timings to the already-seeded genesis.
    /// No slot is changed unless its corresponding `TESTNET_*` knob is present.
    fn patch_staking_timing(&self, tuning: &[(&str, String)]) -> Result<()> {
        let unbonding = tuned_optional(tuning, "TESTNET_UNBONDING_PERIOD_SECS");
        let slashed = tuned_optional(tuning, "TESTNET_SLASHED_WITHDRAWAL_DELAY_SECS");
        if unbonding.is_none() && slashed.is_none() {
            return Ok(());
        }

        let path = self.cfg.dir.join("genesis.json");
        let mut genesis: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path)?)?;
        patch_staking_storage(&mut genesis, unbonding, slashed)?;
        fs::write(&path, serde_json::to_string_pretty(&genesis)? + "\n")?;
        Ok(())
    }
}

fn patch_felony_storage(storage: &mut serde_json::Map<String, serde_json::Value>) {
    let threshold = json!(format!("0x{DEV_FELONY_THRESHOLD:064x}"));
    storage.insert(format!("0x{PROPOSER_FELONY_SLOT:064x}"), threshold.clone());
    storage.insert(format!("0x{VOTER_FELONY_SLOT:064x}"), threshold);
}

/// A `TESTNET_*` tuning override parsed as `u64`, or `default`.
fn tuned(tuning: &[(&str, String)], key: &str, default: u64) -> u64 {
    tuning
        .iter()
        .find(|(k, _)| *k == key)
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(default)
}

fn tuned_optional(tuning: &[(&str, String)], key: &str) -> Option<u64> {
    tuning
        .iter()
        .find(|(candidate, _)| *candidate == key)
        .and_then(|(_, value)| value.parse().ok())
}

fn patch_staking_storage(
    genesis: &mut serde_json::Value,
    unbonding: Option<u64>,
    slashed: Option<u64>,
) -> Result<()> {
    let alloc = genesis
        .get_mut("alloc")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| eyre!("genesis has no alloc object"))?;
    let key = alloc
        .keys()
        .find(|key| address_has_suffix(key, STAKING_SUFFIX))
        .cloned()
        .ok_or_else(|| eyre!("seeded genesis has no Staking alloc entry"))?;
    let storage = alloc
        .get_mut(&key)
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|entry| entry.get_mut("storage"))
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| eyre!("Staking alloc entry has no storage object"))?;
    if let Some(value) = unbonding {
        storage.insert(format!("0x{:064x}", 1u64), json!(format!("0x{value:064x}")));
    }
    if let Some(value) = slashed {
        storage.insert(
            format!("0x{:064x}", 11u64),
            json!(format!("0x{value:064x}")),
        );
    }
    Ok(())
}

/// Whether an alloc key normalizes (lowercase, `0x`-stripped, left-padded to 40)
/// to a SlashIndicator address ending in `ee01` (`bootstrap-testnet.sh:244`).
fn ends_with_ee01(key: &str) -> bool {
    address_has_suffix(key, "ee01")
}

fn address_has_suffix(key: &str, suffix: &str) -> bool {
    let k = key.to_lowercase();
    let k = k.strip_prefix("0x").unwrap_or(&k);
    format!("{k:0>40}").ends_with(suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded_genesis() -> serde_json::Value {
        json!({
            "alloc": {
                "000000000000000000000000000000000000ee02": {
                    "storage": {
                        format!("0x{:064x}", 1u64): format!("0x{:064x}", 1_814_400u64),
                        format!("0x{:064x}", 11u64): format!("0x{:064x}", 3_628_800u64)
                    }
                }
            }
        })
    }

    #[test]
    fn lifecycle_timing_patch_updates_only_requested_staking_slots() {
        let mut genesis = seeded_genesis();
        patch_staking_storage(&mut genesis, Some(8), None).unwrap();
        let storage = genesis["alloc"]["000000000000000000000000000000000000ee02"]["storage"]
            .as_object()
            .unwrap();
        assert_eq!(
            storage[&format!("0x{:064x}", 1u64)],
            json!(format!("0x{:064x}", 8u64))
        );
        assert_eq!(
            storage[&format!("0x{:064x}", 11u64)],
            json!(format!("0x{:064x}", 3_628_800u64))
        );
    }

    #[test]
    fn lifecycle_timing_patch_rejects_unseeded_staking_entry() {
        let mut genesis = json!({ "alloc": {} });
        assert!(patch_staking_storage(&mut genesis, Some(8), None).is_err());
    }

    #[test]
    fn felony_patch_uses_current_slashindicator_config_slots() {
        let mut storage = serde_json::Map::new();
        patch_felony_storage(&mut storage);
        let expected = json!(format!("0x{DEV_FELONY_THRESHOLD:064x}"));
        assert_eq!(
            storage.get(&format!("0x{PROPOSER_FELONY_SLOT:064x}")),
            Some(&expected)
        );
        assert_eq!(
            storage.get(&format!("0x{VOTER_FELONY_SLOT:064x}")),
            Some(&expected)
        );
        assert!(storage.get(&format!("0x{:064x}", 13u64)).is_none());
    }
}
