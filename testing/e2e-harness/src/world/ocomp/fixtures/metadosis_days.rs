//! Fresh-genesis Metadosis days: clearing seeded days and seeding Oracle input.

#[cfg(feature = "ocomp-integration")]
use crate::world::ocomp::*;

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::ocomp) fn clear_seeded_metadosis_days(
    genesis: &mut serde_json::Value,
    chain_id: u64,
) -> Result<bool> {
    let mut provider = HashMapStorageProvider::new(chain_id);
    {
        let alloc = genesis
            .get_mut("alloc")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| eyre::eyre!("generated genesis has no alloc object"))?;
        let metadosis_key = find_alloc_address_key(alloc, METADOSIS_ADDRESS)?
            .ok_or_else(|| eyre::eyre!("generated genesis has no Metadosis account"))?;
        let words = alloc
            .get_mut(&metadosis_key)
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| eyre::eyre!("Metadosis genesis account is not an object"))?
            .entry("storage".to_owned())
            .or_insert_with(|| serde_json::json!({}))
            .as_object()
            .ok_or_else(|| eyre::eyre!("Metadosis genesis account storage is not an object"))?;
        for (slot, value) in words {
            provider.storage.insert(
                (METADOSIS_ADDRESS, parse_hex_word(slot)?),
                parse_storage_word(value)?,
            );
        }
    }

    let seeded = crate::world::localnet::worldwide_day()
        .parse::<WorldwideDay>()
        .map_err(|error| eyre::eyre!("invalid fixture WorldwideDay: {error}"))?;
    StorageHandle::enter(&mut provider, |storage| {
        FreshDevnetGenesisBuilder::new()
            .clear_single_offering_day(seeded)
            .apply(storage)
    })?;

    let alloc = genesis
        .get_mut("alloc")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| eyre::eyre!("generated genesis has no alloc object"))?;
    let metadosis_key = find_alloc_address_key(alloc, METADOSIS_ADDRESS)?
        .ok_or_else(|| eyre::eyre!("generated genesis has no Metadosis account"))?;
    let words = alloc
        .get_mut(&metadosis_key)
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|account| account.get_mut("storage"))
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| eyre::eyre!("Metadosis genesis account has no storage object"))?;
    for ((address, slot), value) in &provider.storage {
        if *address != METADOSIS_ADDRESS {
            continue;
        }
        let slot = format!("0x{slot:064x}");
        if value.is_zero() {
            words.remove(&slot);
        } else {
            words.insert(slot, serde_json::Value::String(format!("0x{value:064x}")));
        }
    }
    Ok(true)
}

/// Seed only raw Oracle evidence for the runtime-created fresh WWD.
///
/// Metadosis remains pristine: block 1 creates the day and the production
/// ResolveForming edge computes and stores the exact 50-hour VWAP. The harness
/// supplies one ordinary DAY_TYPE_PAIR observation inside that interval. This
/// way, the subsequent public Tribute has a real canonical price instead of
/// relying on a pre-materialized WWD snapshot.
#[cfg(feature = "ocomp-integration")]
pub(in crate::world::ocomp) fn seed_fresh_metadosis_oracle_input(
    genesis: &mut serde_json::Value,
    chain_id: u64,
) -> Result<bool> {
    let genesis_timestamp = genesis
        .get("timestamp")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| eyre::eyre!("generated genesis has no timestamp"))
        .and_then(|encoded| u64::try_from(parse_hex_word(encoded)?).map_err(Into::into))?;
    let worldwide_day = WorldwideDay::from_timestamp(genesis_timestamp);
    let oracle_key = {
        let alloc = genesis
            .get("alloc")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| eyre::eyre!("generated genesis has no alloc object"))?;
        find_alloc_address_key(alloc, ORACLE_ADDRESS)?
            .ok_or_else(|| eyre::eyre!("generated genesis has no Oracle account"))?
    };
    let mut provider = HashMapStorageProvider::new(chain_id);
    for (slot, value) in genesis["alloc"][&oracle_key]["storage"]
        .as_object()
        .ok_or_else(|| eyre::eyre!("Oracle genesis account has no storage object"))?
    {
        provider.storage.insert(
            (ORACLE_ADDRESS, parse_hex_word(slot)?),
            parse_storage_word(value)?,
        );
    }
    provider.set_block_number(1);
    StorageHandle::enter(&mut provider, |storage| {
        let pair = outbe_oracle::api::DAY_TYPE_PAIR;
        let current_vwap = U256::from(2);
        let volume = U256::from(1_000_000_u64);
        let mut oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
        // Allow independent production feeders eight blocks to collect quorum.
        // Clock-restart barriers still require a fresh finalized publication.
        // This fixture does not change the production freshness bound.
        oracle
            .config_vote_period
            .write(E2E_ORACLE_VOTE_PERIOD_BLOCKS)?;
        if oracle.pair_index_of(pair)? == 0 {
            return Err(outbe_primitives::error::PrecompileError::Fatal(
                "fresh Metadosis Oracle pair is not registered".into(),
            ));
        }
        oracle.write_snapshot(
            worldwide_day.start_timestamp(),
            &[(pair, current_vwap, volume)],
        )?;
        let scurve = outbe_oracle::api::get_max_active_scurve_value(storage, worldwide_day, pair)?;
        if scurve <= current_vwap {
            return Err(outbe_primitives::error::PrecompileError::Fatal(
                "fresh Metadosis fixture requires an S-curve above its WWD VWAP".into(),
            ));
        }
        Ok(())
    })?;

    let words = genesis["alloc"][&oracle_key]["storage"]
        .as_object_mut()
        .ok_or_else(|| eyre::eyre!("Oracle genesis account has no storage object"))?;
    words.clear();
    for ((address, slot), value) in &provider.storage {
        if *address == ORACLE_ADDRESS && !value.is_zero() {
            words.insert(
                format!("0x{slot:064x}"),
                serde_json::Value::String(format!("0x{value:064x}")),
            );
        }
    }
    Ok(true)
}
