//! Production execution-finalization telemetry of one sealed expiry interval.

use std::collections::BTreeMap;

use alloy_primitives::B256;
use eyre::{ensure, eyre, Result};

use super::{EXPIRED, FCU, FINALIZED};
use crate::features::dkg::frozen::FrozenTarget;

pub(super) struct Telemetry {
    pub finalized: BTreeMap<u64, B256>,
    pub forkchoice: BTreeMap<u64, B256>,
}

pub(super) fn unique_field<'a>(line: &'a str, name: &str) -> Result<&'a str> {
    let prefix = format!("{name}=");
    let mut values = line
        .split_whitespace()
        .filter_map(|word| word.strip_prefix(&prefix));
    let value = values
        .next()
        .ok_or_else(|| eyre!("finalization record omitted {name}"))?;
    ensure!(
        values.next().is_none(),
        "duplicate finalization field {name}"
    );
    Ok(value)
}

pub(super) fn number(value: &str) -> Result<u64> {
    ensure!(
        !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()),
        "invalid telemetry height"
    );
    value
        .parse()
        .map_err(|_| eyre!("telemetry height overflow"))
}

pub(super) fn digest(value: &str) -> Result<B256> {
    ensure!(
        value.len() == 66 && value.starts_with("0x"),
        "invalid finalization digest shape"
    );
    value
        .parse()
        .map_err(|_| eyre!("invalid finalization digest"))
}

pub(super) fn insert_finalized(
    map: &mut BTreeMap<u64, B256>,
    height: u64,
    hash: B256,
    expiry: u64,
) -> Result<()> {
    ensure!(
        height <= expiry,
        "production acknowledged finalization above expiry"
    );
    if let Some(old) = map.insert(height, hash) {
        ensure!(
            old == hash,
            "conflicting finalization digests at one height"
        );
    }
    Ok(())
}

pub(super) fn telemetry(log: &str, target: &FrozenTarget, expiry: u64) -> Result<Telemetry> {
    ensure!(
        log.ends_with('\n') && !log.contains('\u{1b}'),
        "incomplete or colored expiry log interval"
    );
    let mut result = Telemetry {
        finalized: BTreeMap::new(),
        forkchoice: BTreeMap::new(),
    };
    let mut saw_expiry = false;
    for line in log.lines() {
        if line.contains(FINALIZED) || line.contains(FCU) {
            ensure!(
                line.contains(" INFO outbe_consensus::executor::actor::finalization: "),
                "wrong finalization record source"
            );
            let (height, hash, map) = if line.contains(FINALIZED) {
                (
                    number(unique_field(line, "height")?)?,
                    digest(unique_field(line, "digest")?)?,
                    &mut result.finalized,
                )
            } else {
                (
                    number(unique_field(line, "finalized_height")?)?,
                    digest(unique_field(line, "finalized_block_hash")?)?,
                    &mut result.forkchoice,
                )
            };
            insert_finalized(map, height, hash, expiry)?;
        }
        if let Some((_, tail)) = line.split_once(EXPIRED) {
            let (cycle, tail) = tail
                .split_once(", height ")
                .ok_or_else(|| eyre!("malformed expiry cycle"))?;
            let (height, deadline) = tail
                .split_once(", deadline ")
                .ok_or_else(|| eyre!("malformed expiry height"))?;
            ensure!(
                number(cycle)? == target.cycle
                    && number(height)? == expiry
                    && number(deadline)? == expiry,
                "wrong frozen cycle, terminal height or expiry deadline"
            );
            // The same error can also appear in the terminal eyre cause chain.
            // Validate every copy, but only the production tracing record is
            // positive evidence that this stack selected its expiry branch.
            saw_expiry |= line
                .contains(" ERROR outbe_chain::launch::node::consensus: consensus stack failed ");
        }
    }
    ensure!(saw_expiry, "missing exact frozen-target stack error");
    ensure!(
        result.finalized.contains_key(&expiry),
        "missing execution-finalization record at exact expiry"
    );
    for (height, hash) in &result.finalized {
        if let Some(fcu_hash) = result.forkchoice.get(height) {
            ensure!(
                hash == fcu_hash,
                "forkchoice and acknowledgement digests disagree"
            );
        }
    }
    Ok(result)
}
