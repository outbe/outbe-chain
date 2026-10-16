use crate::world::ocomp::*;

impl OcompTopology {
    /// Seed the bounded TRY/EUR Oracle state used by the first Tribute E2E.
    ///
    /// This mutates only the not-yet-started scenario genesis. The production
    /// path still reads the canonical pair registry, WWD snapshot and EUR
    /// S-curve through the ordinary Tribute host/enclave interfaces.
    pub fn prepare_cross_currency_tribute_fixture(&self) -> Result<u32> {
        #[cfg(not(feature = "ocomp-integration"))]
        {
            eyre::bail!("cross-currency Tribute fixture requires ocomp-integration");
        }

        #[cfg(feature = "ocomp-integration")]
        {
            let genesis_path = self.cfg.dir.join("genesis.json");
            let mut genesis: serde_json::Value = serde_json::from_slice(&fs::read(&genesis_path)?)?;
            let chain_id = genesis_chain_id(&genesis)?;
            let configured_offering_secs =
                outbe_chain_constants::GenesisProtocolParametersV1::from_genesis(&genesis)?
                    .metadosis_offering_period_seconds;
            let (_, worldwide_day) = schedule_public_measurement_day(
                &mut genesis,
                chain_id,
                OCOMP_PUBLIC_OFFERING_AFTER_GENESIS_SECS.max(configured_offering_secs),
            )?;
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

            StorageHandle::enter(&mut provider, |storage| {
                let issuance_pair = outbe_oracle::types::AddressPair::new_coen_to(949);
                let reference_pair = outbe_oracle::types::AddressPair::new_coen_to(978);
                let mut oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
                let issuance_index = match oracle.pair_index_of(issuance_pair)? {
                    0 => outbe_oracle::api::register_pair(storage.clone(), issuance_pair)?,
                    index => index,
                };
                let reference_index = match oracle.pair_index_of(reference_pair)? {
                    0 => outbe_oracle::api::register_pair(storage.clone(), reference_pair)?,
                    index => index,
                };
                oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
                if !oracle.reference_currencies.read_all()?.contains(&978) {
                    return Err(outbe_primitives::error::PrecompileError::Fatal(
                        "cross-currency fixture requires EUR in the reference registry".into(),
                    ));
                }
                oracle
                    .worldwide_day_vwap_exists
                    .write(&worldwide_day, true)?;
                oracle
                    .worldwide_day_vwap_start
                    .write(&worldwide_day, worldwide_day.start_timestamp())?;
                oracle.worldwide_day_vwap_end.write(
                    &worldwide_day,
                    worldwide_day.start_timestamp()
                        + outbe_chain_constants::DEFAULT_METADOSIS_FORMING_PERIOD_SECONDS,
                )?;
                let values = oracle.worldwide_day_vwap_value.get_nested(&worldwide_day);
                values.write(&issuance_index, U256::from(10_250_000_u64))?;
                values.write(&reference_index, U256::from(250_000_u64))?;
                outbe_oracle::scurve::store_scurve_entry(
                    &mut oracle,
                    reference_pair,
                    worldwide_day.to_timestamp_utc(),
                    U256::from(320_000_u64),
                )?;
                let inputs =
                    outbe_oracle::api::tribute_pricing_inputs(storage, 949, 978, worldwide_day)?
                        .ok_or_else(|| {
                            outbe_primitives::error::PrecompileError::Fatal(
                                "cross-currency issuance pair was not registered".into(),
                            )
                        })?;
                if inputs.issuance_wwd_vwap_minor != U256::from(10_250_000_u64)
                    || inputs.reference_wwd_vwap_minor != U256::from(250_000_u64)
                    || inputs.reference_scurve_minor != U256::from(320_000_u64)
                {
                    return Err(outbe_primitives::error::PrecompileError::Fatal(
                        "cross-currency fixture did not persist its exact Oracle inputs".into(),
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
            replace_json_atomically(&genesis_path, &genesis)?;
            Ok(worldwide_day.value())
        }
    }
}

pub const METADOSIS_STORAGE_LAYOUT_V1_HASH_HEX: &str =
    "0xb929da1d5f7064f33f1cf515babe3f581aebe3c0ad8ae5baf7903af692001fe6";

/// Provisional block envelope used by the disposable OCM-25 measurement chain.
#[cfg(feature = "ocomp-integration")]
const OCOMP_MEASUREMENT_BLOCK_GAS_LIMIT: u64 = 40_000_000;

#[cfg(feature = "ocomp-integration")]
pub(crate) const OCOMP_PUBLIC_OFFERING_AFTER_GENESIS_SECS: u64 = 600;

#[cfg(feature = "ocomp-integration")]
pub(crate) const OCOMP_CAPACITY_OFFERING_AFTER_GENESIS_SECS: u64 = 3_600;

#[cfg(feature = "ocomp-integration")]
pub(crate) const OCOMP_PUBLIC_TRIBUTE_AMOUNT_BASE: &str = "2";

#[cfg(feature = "ocomp-integration")]
pub(crate) const OCOMP_PUBLIC_TRIBUTE_AMOUNT_MICRO: &str = "0";

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::ocomp) fn parse_outbe_chain_spec(
    path: &Path,
) -> Result<Arc<reth_chainspec::ChainSpec<OutbeHeader>>> {
    let path = path
        .to_str()
        .ok_or_else(|| eyre::eyre!("genesis path is not valid UTF-8"))?;
    Ok(reth_ethereum::cli::chainspec::chain_value_parser(path)?
        .as_ref()
        .clone()
        .map_header(OutbeHeader::new)
        .into())
}

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::ocomp) fn genesis_chain_id(genesis: &serde_json::Value) -> Result<u64> {
    let value = genesis
        .get("config")
        .and_then(|config| config.get("chainId"))
        .ok_or_else(|| eyre::eyre!("generated genesis has no config.chainId"))?;
    match value {
        serde_json::Value::Number(number) => number
            .as_u64()
            .ok_or_else(|| eyre::eyre!("genesis chainId is outside u64")),
        serde_json::Value::String(encoded) => {
            let encoded = encoded.strip_prefix("0x").unwrap_or(encoded);
            u64::from_str_radix(encoded, 16).map_err(Into::into)
        }
        _ => {
            eyre::bail!("genesis chainId is neither a number nor a hex string");
        }
    }
}

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::ocomp) fn schedule_public_measurement_day(
    genesis: &mut serde_json::Value,
    chain_id: u64,
    offering_after_genesis_secs: u64,
) -> Result<(bool, WorldwideDay)> {
    eyre::ensure!(
        offering_after_genesis_secs > 0,
        "OCOMP public measurement offering duration must be non-zero"
    );
    let genesis_timestamp = genesis
        .get("timestamp")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| eyre::eyre!("generated genesis has no timestamp"))
        .and_then(|encoded| u64::try_from(parse_hex_word(encoded)?).map_err(Into::into))?;
    let worldwide_day = WorldwideDay::from_timestamp(genesis_timestamp);
    let forming_start = worldwide_day.start_timestamp();
    eyre::ensure!(
        forming_start < genesis_timestamp,
        "OCOMP public measurement genesis timestamp must follow WorldwideDay start"
    );
    let offering_end = genesis_timestamp
        .checked_add(offering_after_genesis_secs)
        .ok_or_else(|| eyre::eyre!("OCOMP public measurement offering end overflow"))?;
    let day = MeasurementDay {
        worldwide_day,
        forming_start,
        genesis_timestamp,
        offering_end,
    };

    let mut provider = HashMapStorageProvider::new(chain_id);
    let account_keys = load_measurement_accounts(genesis, &mut provider)?;
    provider.set_block_number(1);
    let changed = StorageHandle::enter(&mut provider, |storage| {
        seed_public_measurement_day(storage, &day)
    })?;
    write_measurement_accounts(genesis, &provider, account_keys)?;
    Ok((changed, worldwide_day))
}

/// The public measurement WorldwideDay, offering from genesis until its end.
#[cfg(feature = "ocomp-integration")]
struct MeasurementDay {
    worldwide_day: WorldwideDay,
    forming_start: u64,
    genesis_timestamp: u64,
    offering_end: u64,
}

#[cfg(feature = "ocomp-integration")]
type PrecompileResult<T> = std::result::Result<T, outbe_primitives::error::PrecompileError>;

#[cfg(feature = "ocomp-integration")]
fn fatal(message: &str) -> outbe_primitives::error::PrecompileError {
    outbe_primitives::error::PrecompileError::Fatal(message.into())
}

/// Loads the Metadosis, Oracle and Tribute genesis storage; returns each
/// account's alloc key for the write-back.
#[cfg(feature = "ocomp-integration")]
fn load_measurement_accounts(
    genesis: &serde_json::Value,
    provider: &mut HashMapStorageProvider,
) -> Result<Vec<(Address, String)>> {
    let alloc = genesis
        .get("alloc")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| eyre::eyre!("generated genesis has no alloc object"))?;
    let mut keys = Vec::with_capacity(3);
    for (address, label) in [
        (METADOSIS_ADDRESS, "Metadosis"),
        (ORACLE_ADDRESS, "Oracle"),
        (TRIBUTE_ADDRESS, "Tribute"),
    ] {
        let account_key = find_alloc_address_key(alloc, address)?
            .ok_or_else(|| eyre::eyre!("generated genesis has no {label} account"))?;
        let account = alloc
            .get(&account_key)
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| eyre::eyre!("{label} genesis account is not an object"))?;
        let words = account
            .get("storage")
            .map(|storage| {
                storage
                    .as_object()
                    .ok_or_else(|| eyre::eyre!("{label} genesis account storage is not an object"))
            })
            .transpose()?;
        for (slot, value) in words.into_iter().flatten() {
            provider
                .storage
                .insert((address, parse_hex_word(slot)?), parse_storage_word(value)?);
        }
        keys.push((address, account_key));
    }
    Ok(keys)
}

#[cfg(feature = "ocomp-integration")]
fn seed_public_measurement_day(
    storage: StorageHandle<'_>,
    day: &MeasurementDay,
) -> PrecompileResult<bool> {
    let pair = outbe_oracle::api::DAY_TYPE_PAIR;
    let seeded_rate =
        outbe_oracle::api::get_exchange_rate(storage.clone(), pair.address1(), pair.address2())?;
    if seeded_rate.is_zero() {
        return Err(fatal(
            "OCOMP public measurement Oracle seed has zero COEN/0xUSD rate",
        ));
    }
    let worldwide_day = day.worldwide_day;
    let previous_day = worldwide_day.previous_date_key();
    write_measurement_vwaps(storage.clone(), day)?;
    let stored_previous = outbe_oracle::api::day_type_pair_vwap(storage.clone(), previous_day)?
        .ok_or_else(|| fatal("OCOMP public measurement previous VWAP is missing"))?;
    let stored_current = outbe_oracle::api::day_type_pair_vwap(storage.clone(), worldwide_day)?
        .ok_or_else(|| fatal("OCOMP public measurement current VWAP is missing"))?;
    let day_type = if stored_current > stored_previous {
        WwdDayType::Green
    } else {
        WwdDayType::Red
    };
    let entry_price = stored_current.max(outbe_oracle::api::get_max_active_scurve_value(
        storage.clone(),
        worldwide_day,
        pair,
    )?);
    let qualification_rate = entry_price
        .checked_mul(U256::from(2))
        .ok_or_else(|| fatal("OCOMP public measurement qualification rate overflow"))?;
    outbe_oracle::api::set_exchange_rate(
        storage.clone(),
        Address::ZERO,
        pair,
        outbe_oracle::api::RateObservation {
            rate: qualification_rate,
            block_number: 1,
            timestamp: day.genesis_timestamp,
        },
    )?;
    let day_limit = U256::from(500) * outbe_primitives::units::SCALE_1E6_U256;
    let report = FreshDevnetGenesisBuilder::new()
        .seed_active_worldwide_day(GenesisWorldwideDay {
            worldwide_day,
            status: WwdStatus::Offering,
            day_type,
            forming_start: day.forming_start,
            forming_end: day.genesis_timestamp,
            lookback_end: day.genesis_timestamp,
            offering_end: day.offering_end,
            scheduled_process_time: day.offering_end,
            metadosis_limit_minor: day_limit,
            previous_vwap: stored_previous,
            current_vwap: stored_current,
        })
        .apply(storage.clone())?;
    outbe_tribute::TributeContract::new(storage).unseal_day(worldwide_day)?;
    Ok(report.changed)
}

/// Keep this fixture's ordinary mineGratis path independent from the
/// stablecoin/vault scenarios. Together with the Tribute amount, these prices
/// produce non-zero Gratis and zero mining cost.
#[cfg(feature = "ocomp-integration")]
fn write_measurement_vwaps(
    storage: StorageHandle<'_>,
    day: &MeasurementDay,
) -> PrecompileResult<()> {
    let pair = outbe_oracle::api::DAY_TYPE_PAIR;
    let previous_vwap = U256::ONE;
    let current_vwap = U256::from(2);
    let previous_day = day.worldwide_day.previous_date_key();
    let previous_sample_time = day
        .forming_start
        .checked_sub(1)
        .ok_or_else(|| fatal("OCOMP public measurement previous VWAP window underflow"))?;
    let volume = U256::from(1_000_000_u64);
    let mut oracle = outbe_oracle::schema::OracleContract::new(storage);
    let inherited_scurve_expiry = day
        .genesis_timestamp
        .checked_add(
            (outbe_oracle::scurve::PERIOD as u64 + 1) * outbe_primitives::time::SECONDS_PER_DAY,
        )
        .ok_or_else(|| fatal("OCOMP public measurement S-curve expiry overflow"))?;
    outbe_oracle::scurve::evict_expired_scurves(&mut oracle, inherited_scurve_expiry)?;
    oracle.write_snapshot(previous_sample_time, &[(pair, previous_vwap, volume)])?;
    oracle.write_snapshot(day.forming_start, &[(pair, current_vwap, volume)])?;
    let pair_index = oracle.pair_index_of(pair)?;
    if pair_index == 0 {
        return Err(fatal(
            "OCOMP public measurement Oracle pair is not registered",
        ));
    }
    for (vwap_day, vwap) in [
        (previous_day, previous_vwap),
        (day.worldwide_day, current_vwap),
    ] {
        let start = vwap_day.start_timestamp();
        let end = start
            .checked_add(outbe_chain_constants::DEFAULT_METADOSIS_FORMING_PERIOD_SECONDS)
            .ok_or_else(|| fatal("OCOMP public measurement WWD window overflow"))?;
        oracle.worldwide_day_vwap_exists.write(&vwap_day, true)?;
        oracle.worldwide_day_vwap_start.write(&vwap_day, start)?;
        oracle.worldwide_day_vwap_end.write(&vwap_day, end)?;
        oracle
            .worldwide_day_vwap_value
            .get_nested(&vwap_day)
            .write(&pair_index, vwap)?;
    }
    Ok(())
}

#[cfg(feature = "ocomp-integration")]
fn write_measurement_accounts(
    genesis: &mut serde_json::Value,
    provider: &HashMapStorageProvider,
    account_keys: Vec<(Address, String)>,
) -> Result<()> {
    let alloc = genesis
        .get_mut("alloc")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| eyre::eyre!("generated genesis has no alloc object"))?;
    for (address, account_key) in account_keys {
        let account = alloc
            .get_mut(&account_key)
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| eyre::eyre!("genesis account {address:#x} is not an object"))?;
        let words = account
            .entry("storage".to_owned())
            .or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
            .ok_or_else(|| eyre::eyre!("genesis account {address:#x} storage is not an object"))?;
        words.clear();
        for ((stored_address, slot), value) in &provider.storage {
            if *stored_address == address && !value.is_zero() {
                words.insert(
                    format!("0x{slot:064x}"),
                    serde_json::Value::String(format!("0x{value:064x}")),
                );
            }
        }
    }
    Ok(())
}

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::ocomp) fn apply_measurement_gas_envelope(
    genesis: &mut serde_json::Value,
) -> Result<bool> {
    let object = genesis
        .as_object_mut()
        .ok_or_else(|| eyre::eyre!("measurement genesis must be a JSON object"))?;
    let expected = serde_json::Value::String(format!("0x{OCOMP_MEASUREMENT_BLOCK_GAS_LIMIT:x}"));
    if object.get("gasLimit") == Some(&expected) {
        return Ok(false);
    }
    object.insert("gasLimit".to_owned(), expected);
    Ok(true)
}

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::ocomp) fn find_alloc_address_key(
    alloc: &serde_json::Map<String, serde_json::Value>,
    expected: Address,
) -> Result<Option<String>> {
    for key in alloc.keys() {
        let normalized = if key.starts_with("0x") {
            key.clone()
        } else {
            format!("0x{key}")
        };
        if Address::from_str(&normalized)
            .map_err(|error| eyre::eyre!("invalid genesis alloc address {key}: {error}"))?
            == expected
        {
            return Ok(Some(key.clone()));
        }
    }
    Ok(None)
}

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::ocomp) fn parse_storage_word(value: &serde_json::Value) -> Result<U256> {
    let encoded = value
        .as_str()
        .ok_or_else(|| eyre::eyre!("genesis storage word is not a string"))?;
    parse_hex_word(encoded)
}

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::ocomp) fn parse_hex_word(encoded: &str) -> Result<U256> {
    U256::from_str_radix(encoded.strip_prefix("0x").unwrap_or(encoded), 16).map_err(Into::into)
}
