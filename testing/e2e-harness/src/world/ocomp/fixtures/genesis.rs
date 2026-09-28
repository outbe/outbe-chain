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
    "0x193b70d52eaf69583d3407af7281cbff732334fb32992ee0be69404a841c468a";

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

    let mut provider = HashMapStorageProvider::new(chain_id);
    let account_keys = {
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
            if let Some(storage) = account.get("storage") {
                let words = storage.as_object().ok_or_else(|| {
                    eyre::eyre!("{label} genesis account storage is not an object")
                })?;
                for (slot, value) in words {
                    provider
                        .storage
                        .insert((address, parse_hex_word(slot)?), parse_storage_word(value)?);
                }
            }
            keys.push((address, account_key));
        }
        keys
    };

    provider.set_block_number(1);
    let changed = StorageHandle::enter(&mut provider, |storage| {
        let pair = outbe_oracle::api::DAY_TYPE_PAIR;
        let seeded_rate = outbe_oracle::api::get_exchange_rate(
            storage.clone(),
            pair.address1(),
            pair.address2(),
        )?;
        if seeded_rate.is_zero() {
            return Err(outbe_primitives::error::PrecompileError::Fatal(
                "OCOMP public measurement Oracle seed has zero COEN/0xUSD rate".into(),
            ));
        }
        // Keep this fixture's ordinary mineGratis path independent from the
        // stablecoin/vault scenarios. Together with the Tribute
        // amount, these prices produce non-zero Gratis and zero mining cost.
        let previous_vwap = U256::ONE;
        let current_vwap = U256::from(2);
        let previous_day = worldwide_day.previous_date_key();
        let previous_sample_time = forming_start.checked_sub(1).ok_or_else(|| {
            outbe_primitives::error::PrecompileError::Fatal(
                "OCOMP public measurement previous VWAP window underflow".into(),
            )
        })?;
        let volume = U256::from(1_000_000_u64);
        let mut oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
        let inherited_scurve_expiry = genesis_timestamp
            .checked_add(
                (outbe_oracle::scurve::PERIOD as u64 + 1) * outbe_oracle::scurve::DAY_SECONDS,
            )
            .ok_or_else(|| {
                outbe_primitives::error::PrecompileError::Fatal(
                    "OCOMP public measurement S-curve expiry overflow".into(),
                )
            })?;
        outbe_oracle::scurve::evict_expired_scurves(&mut oracle, inherited_scurve_expiry)?;
        oracle.write_snapshot(previous_sample_time, &[(pair, previous_vwap, volume)])?;
        oracle.write_snapshot(forming_start, &[(pair, current_vwap, volume)])?;
        let pair_index = oracle.pair_index_of(pair)?;
        if pair_index == 0 {
            return Err(outbe_primitives::error::PrecompileError::Fatal(
                "OCOMP public measurement Oracle pair is not registered".into(),
            ));
        }
        for (day, vwap) in [(previous_day, previous_vwap), (worldwide_day, current_vwap)] {
            let start = day.start_timestamp();
            let end = start
                .checked_add(outbe_chain_constants::DEFAULT_METADOSIS_FORMING_PERIOD_SECONDS)
                .ok_or_else(|| {
                    outbe_primitives::error::PrecompileError::Fatal(
                        "OCOMP public measurement WWD window overflow".into(),
                    )
                })?;
            oracle.worldwide_day_vwap_exists.write(&day, true)?;
            oracle.worldwide_day_vwap_start.write(&day, start)?;
            oracle.worldwide_day_vwap_end.write(&day, end)?;
            oracle
                .worldwide_day_vwap_value
                .get_nested(&day)
                .write(&pair_index, vwap)?;
        }
        let stored_previous = outbe_oracle::api::day_type_pair_vwap(storage.clone(), previous_day)?
            .ok_or_else(|| {
                outbe_primitives::error::PrecompileError::Fatal(
                    "OCOMP public measurement previous VWAP is missing".into(),
                )
            })?;
        let stored_current = outbe_oracle::api::day_type_pair_vwap(storage.clone(), worldwide_day)?
            .ok_or_else(|| {
                outbe_primitives::error::PrecompileError::Fatal(
                    "OCOMP public measurement current VWAP is missing".into(),
                )
            })?;
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
        let qualification_rate = entry_price.checked_mul(U256::from(2)).ok_or_else(|| {
            outbe_primitives::error::PrecompileError::Fatal(
                "OCOMP public measurement qualification rate overflow".into(),
            )
        })?;
        outbe_oracle::api::set_exchange_rate(
            storage.clone(),
            Address::ZERO,
            pair,
            qualification_rate,
            1,
            genesis_timestamp,
        )?;
        let day_limit = U256::from(500) * outbe_primitives::units::SCALE_1E6_U256;
        let report = FreshDevnetGenesisBuilder::new()
            .seed_active_worldwide_day(GenesisWorldwideDay {
                worldwide_day,
                status: WwdStatus::Offering,
                day_type,
                forming_start,
                forming_end: genesis_timestamp,
                lookback_end: genesis_timestamp,
                offering_end,
                scheduled_process_time: offering_end,
                metadosis_limit_amount: day_limit,
                previous_vwap: stored_previous,
                current_vwap: stored_current,
            })
            .apply(storage.clone())?;
        outbe_tribute::TributeContract::new(storage).unseal_day(worldwide_day)?;
        Ok(report.changed)
    })?;

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
    Ok((changed, worldwide_day))
}

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
/// supplies one ordinary DAY_TYPE_PAIR observation inside that interval so the
/// subsequent public Tribute has a real canonical price instead of relying on a
/// pre-materialized WWD snapshot.
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
        // Clock-restart barriers still require a fresh finalized publication;
        // this fixture does not change the production freshness bound.
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

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::ocomp) fn capacity_tribute_private_keys(count: usize) -> Result<Vec<String>> {
    const FIRST_CAPACITY_SCALAR: u64 = 0x1_0000;

    let mut private_keys = Vec::new();
    private_keys.try_reserve_exact(count)?;
    for index in 0..count {
        let scalar = FIRST_CAPACITY_SCALAR
            .checked_add(u64::try_from(index)?)
            .ok_or_else(|| eyre::eyre!("capacity Tribute owner scalar overflow"))?;
        let mut bytes = [0_u8; 32];
        bytes[24..].copy_from_slice(&scalar.to_be_bytes());
        SigningKey::from_bytes((&bytes).into())
            .map_err(|error| eyre::eyre!("invalid capacity Tribute owner scalar: {error}"))?;
        private_keys.push(format!("0x{}", hex::encode(bytes)));
    }
    Ok(private_keys)
}

/// Seed explicitly listed bulk operators through the production registry API
/// before launch, avoiding a governance window per owner. Returns whether the
/// genesis changed; existing registrations must match the requested fixture.
///
/// Each owner gets a fixture chain id and its deterministic
/// root-signing key, used to sign the offer's Merkle root.
#[cfg(feature = "ocomp-integration")]
pub(in crate::world::ocomp) fn seed_capacity_operator_l2_registrations(
    genesis: &mut serde_json::Value,
    chain_id: u64,
    private_keys: &[String],
) -> Result<bool> {
    use outbe_l2registry::L2RegistryContract;
    use outbe_primitives::addresses::L2_REGISTRY_ADDRESS;

    if private_keys.is_empty() {
        return Ok(false);
    }

    let mut registrations = Vec::with_capacity(private_keys.len());
    for (index, private_key) in private_keys.iter().enumerate() {
        let l1_address = crate::internal::eth::address_of(private_key)
            .ok_or_else(|| eyre::eyre!("cannot derive bulk Tribute owner"))?;
        let operator_chain_id = CAPACITY_OPERATOR_L2_CHAIN_ID_BASE
            .checked_add(u64::try_from(index)?)
            .ok_or_else(|| eyre::eyre!("bulk L2 chain id overflow"))?;
        u32::try_from(operator_chain_id)
            .map_err(|_| eyre::eyre!("bulk L2 chain id exceeds the offer selector width"))?;
        registrations.push((
            operator_chain_id,
            l1_address,
            crate::internal::l2_fixture::root_signing_public_key(operator_chain_id),
        ));
    }

    let mut provider = HashMapStorageProvider::new(chain_id);
    let existing = {
        let alloc = genesis
            .get("alloc")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| eyre::eyre!("generated genesis has no alloc object"))?;
        find_alloc_address_key(alloc, L2_REGISTRY_ADDRESS)?
            .and_then(|key| alloc.get(&key))
            .and_then(|account| account.get("storage"))
            .cloned()
    };
    if let Some(storage) = existing.as_ref().and_then(serde_json::Value::as_object) {
        for (slot, value) in storage {
            provider.storage.insert(
                (L2_REGISTRY_ADDRESS, parse_hex_word(slot)?),
                parse_storage_word(value)?,
            );
        }
    }

    provider.set_block_number(1);
    let changed = StorageHandle::enter(&mut provider, |storage| -> Result<bool> {
        let mut registry = L2RegistryContract::new(storage);
        let mut changed = false;
        for (operator_chain_id, l1_address, public_key) in &registrations {
            if let Some(record) = registry.networks.get(*operator_chain_id)? {
                eyre::ensure!(
                    record.l1_address == *l1_address
                        && record.public_key_bytes()?.as_slice() == public_key.as_slice()
                        && registry.l1_to_chain.read(l1_address)? == *operator_chain_id,
                    "conflicting bulk L2 registration for chain {operator_chain_id}"
                );
            } else {
                registry.register_network(*operator_chain_id, *l1_address, public_key)?;
                changed = true;
            }
        }
        Ok(changed)
    })?;
    if !changed {
        return Ok(false);
    }

    let alloc = genesis
        .get_mut("alloc")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| eyre::eyre!("generated genesis has no alloc object"))?;
    let account_key = match find_alloc_address_key(alloc, L2_REGISTRY_ADDRESS)? {
        Some(key) => key,
        None => {
            let key = format!("{L2_REGISTRY_ADDRESS:x}");
            alloc.insert(
                key.clone(),
                serde_json::json!({ "balance": "0x0", "code": "0xef", "storage": {} }),
            );
            key
        }
    };
    let words = alloc
        .get_mut(&account_key)
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| eyre::eyre!("L2Registry genesis account is not an object"))?
        .entry("storage".to_owned())
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| eyre::eyre!("L2Registry genesis account storage is not an object"))?;
    for ((address, slot), value) in &provider.storage {
        if *address != L2_REGISTRY_ADDRESS || value.is_zero() {
            continue;
        }
        words.insert(
            format!("0x{slot:064x}"),
            serde_json::Value::String(format!("0x{value:064x}")),
        );
    }
    Ok(true)
}

/// Bulk fixture ids use a separate namespace from governed operators.
#[cfg(feature = "ocomp-integration")]
const CAPACITY_OPERATOR_L2_CHAIN_ID_BASE: u64 = 0xE2E1_0000;

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::ocomp) fn fund_capacity_tribute_accounts(
    genesis: &mut serde_json::Value,
    private_keys: &[String],
) -> Result<bool> {
    const CAPACITY_OWNER_BALANCE_COEN: u64 = 1_000;
    const COEN_BASE_UNITS: u64 = 1_000_000_000_000_000_000;

    let alloc = genesis
        .get_mut("alloc")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| eyre::eyre!("generated genesis has no alloc object"))?;
    let balance = U256::from(CAPACITY_OWNER_BALANCE_COEN)
        .checked_mul(U256::from(COEN_BASE_UNITS))
        .ok_or_else(|| eyre::eyre!("capacity Tribute owner balance overflow"))?;
    let balance_hex = format!("{balance:#x}");
    let mut changed = false;
    for private_key in private_keys {
        let owner = crate::internal::eth::address_of(private_key)
            .ok_or_else(|| eyre::eyre!("cannot derive capacity Tribute owner"))?;
        let key = format!("{owner:#x}");
        match alloc.get(&key) {
            Some(existing)
                if existing.as_object().is_some_and(|account| {
                    account.len() == 1
                        && account.get("balance").and_then(serde_json::Value::as_str)
                            == Some(balance_hex.as_str())
                }) => {}
            Some(_) => {
                eyre::bail!(
                    "capacity Tribute owner {owner:#x} collides with a different genesis account"
                );
            }
            None => {
                alloc.insert(
                    key,
                    serde_json::json!({
                        "balance": balance_hex,
                    }),
                );
                changed = true;
            }
        }
    }
    Ok(changed)
}

#[cfg(all(test, feature = "ocomp-integration"))]
mod l2_registration_tests {
    use super::*;

    #[test]
    fn bulk_l2_genesis_seeding_is_idempotent_and_rejects_conflicts() {
        let owner = format!("{:#x}", Address::repeat_byte(0x77));
        let account = serde_json::json!({ "balance": "0x1234" });
        let mut genesis = serde_json::json!({ "alloc": { owner.clone(): account.clone() } });
        let keys = [format!("{:064x}", 1), format!("{:064x}", 2)];
        let chain_id = outbe_primitives::chain::DEVNET_CHAIN_ID;

        assert!(seed_capacity_operator_l2_registrations(&mut genesis, chain_id, &keys).unwrap());
        assert_eq!(genesis["alloc"][&owner], account);
        let seeded = genesis.clone();
        assert!(!seed_capacity_operator_l2_registrations(&mut genesis, chain_id, &keys).unwrap());
        assert_eq!(genesis, seeded);

        let conflicting = [keys[1].clone(), keys[0].clone()];
        assert!(
            seed_capacity_operator_l2_registrations(&mut genesis, chain_id, &conflicting).is_err()
        );
        assert_eq!(genesis, seeded);
    }
}
