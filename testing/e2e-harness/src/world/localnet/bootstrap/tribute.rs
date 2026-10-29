use super::*;
use alloy_primitives::{Address, U256};
use outbe_primitives::{
    addresses::{
        EMISSION_ADDRESS, METADOSIS_ADDRESS, ORACLE_ADDRESS, TRIBUTE_ADDRESS,
        TRIBUTE_FACTORY_ADDRESS,
    },
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
    time::WorldwideDay,
};
use outbe_tributefactory::worldwideday::genesis::{FreshDevnetGenesisBuilder, GenesisWorldwideDay};
use std::str::FromStr as _;

struct OfferingPairs {
    issuance_iso: u16,
    reference_iso: u16,
    issuance_vwap: U256,
    reference_vwap: U256,
    reference_scurve: U256,
}

impl Localnet {
    pub(crate) fn prepare_native_lysis_fixture(&self, worldwide_day: u32) -> Result<()> {
        self.prepare_native_lysis_prices(
            worldwide_day,
            &[(840, U256::from(1_000_000)), (978, U256::from(250_000))],
        )
    }

    /// Same allocation as [`prepare_native_lysis_fixture`], with the entry prices
    /// the scenario's reference and issuance currencies must seal.
    pub(crate) fn prepare_native_lysis_prices(
        &self,
        worldwide_day: u32,
        prices: &[(u16, U256)],
    ) -> Result<()> {
        let path = self.cfg.dir.join("genesis.json");
        let mut genesis: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
        let mut provider = HashMapStorageProvider::new(genesis_chain_id(&genesis)?);
        let keys = load_measurement_accounts(&genesis, &mut provider)?;
        let timestamp = u64::try_from(parse_hex_word(
            genesis["timestamp"]
                .as_str()
                .ok_or_else(|| eyre!("genesis timestamp"))?,
        )?)?;
        provider.set_block_number(1);
        provider.set_timestamp(U256::from(timestamp));
        provider.enable_metadosis_mutation_frames(
            outbe_primitives::storage::MetadosisMutationPurposeTag::CycleLifecycle,
            1,
        );
        StorageHandle::enter(&mut provider, |storage| {
            seed_native_lysis_fixture(storage, WorldwideDay::new(worldwide_day), timestamp, prices)
        })?;
        write_measurement_accounts(&mut genesis, &provider, keys)?;
        replace_json_atomically(&path, &genesis)
    }

    /// Offering day plus a native Lysis allocation for MYR issuance against a USD
    /// reference. `usd_entry_price_minor` is the USD UTC-day VWAP Lysis seals.
    pub(crate) fn prepare_nod_lifecycle_fixture(&self, usd_entry_price_minor: u64) -> Result<u32> {
        let entry = U256::from(usd_entry_price_minor);
        let day = self.prepare_offering_fixture(OfferingPairs {
            issuance_iso: 458,
            reference_iso: 840,
            issuance_vwap: U256::from(10_250_000_u64),
            reference_vwap: entry,
            reference_scurve: entry,
        })?;
        self.prepare_native_lysis_prices(day, &[(840, entry), (458, U256::from(10_250_000_u64))])?;
        Ok(day)
    }

    pub(crate) fn fund_tribute_owners(&self, count: usize) -> Result<Vec<String>> {
        let path = self.cfg.dir.join("genesis.json");
        let mut genesis = serde_json::from_slice(&fs::read(&path)?)?;
        let keys = capacity_tribute_private_keys(count)?;
        fund_capacity_tribute_accounts(&mut genesis, &keys)?;
        replace_json_atomically(&path, &genesis)?;
        Ok(keys)
    }

    /// Seed the bounded TRY/EUR Oracle state used by the first Tribute E2E.
    ///
    /// This mutates only the not-yet-started scenario genesis. The production
    /// path still reads the canonical pair registry, WWD snapshot and EUR
    /// S-curve through the ordinary Tribute host/enclave interfaces.
    pub fn prepare_cross_currency_tribute_fixture(&self) -> Result<u32> {
        self.prepare_offering_fixture(OfferingPairs {
            issuance_iso: 949,
            reference_iso: 978,
            issuance_vwap: U256::from(10_250_000_u64),
            reference_vwap: U256::from(250_000_u64),
            reference_scurve: U256::from(320_000_u64),
        })
    }

    fn prepare_offering_fixture(&self, pairs: OfferingPairs) -> Result<u32> {
        {
            let genesis_path = self.cfg.dir.join("genesis.json");
            let mut genesis: serde_json::Value = serde_json::from_slice(&fs::read(&genesis_path)?)?;
            let chain_id = genesis_chain_id(&genesis)?;
            let parameters =
                outbe_chain_constants::GenesisProtocolParametersV1::from_genesis(&genesis)?;
            let (_, day) = schedule_public_measurement_day(
                &mut genesis,
                chain_id,
                parameters.metadosis_forming_period_seconds,
                PUBLIC_OFFERING_AFTER_GENESIS_SECS
                    .max(parameters.metadosis_offering_period_seconds),
            )?;
            let worldwide_day = day.worldwide_day;
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
                let issuance_pair =
                    outbe_oracle::types::AddressPair::new_coen_to(pairs.issuance_iso);
                let reference_pair =
                    outbe_oracle::types::AddressPair::new_coen_to(pairs.reference_iso);
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
                oracle
                    .worldwide_day_vwap_exists
                    .write(&worldwide_day, true)?;
                oracle
                    .worldwide_day_vwap_start
                    .write(&worldwide_day, day.forming_start)?;
                oracle
                    .worldwide_day_vwap_end
                    .write(&worldwide_day, day.forming_end)?;
                let values = oracle.worldwide_day_vwap_value.get_nested(&worldwide_day);
                values.write(&issuance_index, pairs.issuance_vwap)?;
                values.write(&reference_index, pairs.reference_vwap)?;
                outbe_oracle::scurve::store_scurve_entry(
                    &mut oracle,
                    reference_pair,
                    worldwide_day,
                    pairs.reference_scurve,
                )?;
                let inputs = outbe_oracle::api::tribute_pricing_inputs(
                    storage,
                    pairs.issuance_iso,
                    pairs.reference_iso,
                    worldwide_day,
                )?
                .ok_or_else(|| {
                    outbe_primitives::error::PrecompileError::Fatal(
                        "cross-currency issuance pair was not registered".into(),
                    )
                })?;
                if inputs.issuance_wwd_vwap_minor != pairs.issuance_vwap
                    || inputs.reference_wwd_vwap_minor != pairs.reference_vwap
                    || inputs.reference_scurve_minor != pairs.reference_scurve
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

const PUBLIC_OFFERING_AFTER_GENESIS_SECS: u64 = 600;
fn genesis_chain_id(genesis: &serde_json::Value) -> Result<u64> {
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

fn schedule_public_measurement_day(
    genesis: &mut serde_json::Value,
    chain_id: u64,
    forming_period_seconds: u64,
    offering_after_genesis_secs: u64,
) -> Result<(bool, MeasurementDay)> {
    let genesis_timestamp = genesis
        .get("timestamp")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| eyre::eyre!("generated genesis has no timestamp"))
        .and_then(|encoded| u64::try_from(parse_hex_word(encoded)?).map_err(Into::into))?;
    let day = MeasurementDay::new(
        genesis_timestamp,
        forming_period_seconds,
        offering_after_genesis_secs,
    )?;

    let mut provider = HashMapStorageProvider::new(chain_id);
    let account_keys = load_measurement_accounts(genesis, &mut provider)?;
    provider.set_block_number(1);
    provider.set_timestamp(U256::from(genesis_timestamp));
    provider.enable_metadosis_mutation_frames(
        outbe_primitives::storage::MetadosisMutationPurposeTag::CycleLifecycle,
        1,
    );
    let changed = StorageHandle::enter(&mut provider, |storage| {
        seed_public_measurement_day(storage, &day)
    })?;
    write_measurement_accounts(genesis, &provider, account_keys)?;
    Ok((changed, day))
}

/// The public measurement WorldwideDay, offering from genesis until its end.
struct MeasurementDay {
    worldwide_day: WorldwideDay,
    forming_start: u64,
    forming_end: u64,
    genesis_timestamp: u64,
    offering_end: u64,
}

impl MeasurementDay {
    fn new(
        genesis_timestamp: u64,
        forming_period_seconds: u64,
        offering_after_genesis_secs: u64,
    ) -> Result<Self> {
        eyre::ensure!(
            forming_period_seconds > 0,
            "forming duration must be non-zero"
        );
        eyre::ensure!(
            offering_after_genesis_secs > 0,
            "native Tribute fixture offering duration must be non-zero"
        );
        let mut worldwide_day = WorldwideDay::new(outbe_primitives::time::timestamp_to_date_key(
            genesis_timestamp,
        ))
        .previous_date_key()
        .map_err(|error| eyre!("native Tribute fixture date: {error}"))?;
        let (forming_start, forming_end) = loop {
            let start = worldwide_day.start_timestamp();
            let end = start
                .checked_add(forming_period_seconds)
                .ok_or_else(|| eyre!("native Tribute fixture forming end overflow"))?;
            if end <= genesis_timestamp {
                break (start, end);
            }
            worldwide_day = worldwide_day
                .previous_date_key()
                .map_err(|error| eyre!("native Tribute fixture date: {error}"))?;
        };
        let offering_end = genesis_timestamp
            .checked_add(offering_after_genesis_secs)
            .ok_or_else(|| eyre!("native Tribute fixture offering end overflow"))?;
        Ok(Self {
            worldwide_day,
            forming_start,
            forming_end,
            genesis_timestamp,
            offering_end,
        })
    }
}

type PrecompileResult<T> = std::result::Result<T, outbe_primitives::error::PrecompileError>;

fn fatal(message: &str) -> outbe_primitives::error::PrecompileError {
    outbe_primitives::error::PrecompileError::Fatal(message.into())
}

/// Loads the Metadosis, Oracle and Tribute genesis storage. Returns each
/// account's alloc key for the write-back.
fn load_measurement_accounts(
    genesis: &serde_json::Value,
    provider: &mut HashMapStorageProvider,
) -> Result<Vec<(Address, String)>> {
    let alloc = genesis
        .get("alloc")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| eyre::eyre!("generated genesis has no alloc object"))?;
    let mut keys = Vec::with_capacity(5);
    for (address, label) in [
        (EMISSION_ADDRESS, "Emission"),
        (METADOSIS_ADDRESS, "Metadosis"),
        (ORACLE_ADDRESS, "Oracle"),
        (TRIBUTE_ADDRESS, "Tribute"),
        (TRIBUTE_FACTORY_ADDRESS, "TributeFactory"),
    ] {
        let Some(account_key) = find_alloc_address_key(alloc, address)? else {
            if matches!(address, EMISSION_ADDRESS | TRIBUTE_FACTORY_ADDRESS) {
                keys.push((address, format!("{address:#x}")));
                continue;
            }
            eyre::bail!("generated genesis has no {label} account");
        };
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

fn seed_public_measurement_day(
    storage: StorageHandle<'_>,
    day: &MeasurementDay,
) -> PrecompileResult<bool> {
    let pair = outbe_oracle::api::DAY_TYPE_PAIR;
    let seeded_rate =
        outbe_oracle::api::get_exchange_rate(storage.clone(), pair.address1(), pair.address2())?;
    if seeded_rate.is_zero() {
        return Err(fatal(
            "native Tribute fixture Oracle seed has zero COEN/0xUSD rate",
        ));
    }
    let worldwide_day = day.worldwide_day;
    write_measurement_vwaps(storage.clone(), day)?;
    let stored_current = outbe_oracle::api::day_type_pair_vwap(storage.clone(), worldwide_day)?
        .ok_or_else(|| fatal("native Tribute fixture current VWAP is missing"))?;
    let entry_price = stored_current.max(outbe_oracle::api::get_scurve_value(
        storage.clone(),
        worldwide_day,
        pair,
    )?);
    let qualification_rate = entry_price
        .checked_mul(U256::from(2))
        .ok_or_else(|| fatal("native Tribute fixture qualification rate overflow"))?;
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
    let report = FreshDevnetGenesisBuilder::new()
        .seed_active_worldwide_day(GenesisWorldwideDay {
            worldwide_day,
            day: outbe_primitives::time::timestamp_to_date_key(day.offering_end),
            forming_start: day.forming_start,
            forming_end: day.forming_end,
            offerings_start: day.genesis_timestamp,
            offering_end: day.offering_end,
        })
        .apply(storage)?;
    Ok(report.changed)
}

fn seed_native_lysis_fixture(
    storage: StorageHandle<'_>,
    worldwide_day: WorldwideDay,
    timestamp: u64,
    prices: &[(u16, U256)],
) -> Result<()> {
    let projection =
        outbe_tributefactory::worldwideday::api::worldwide_day(storage.clone(), worldwide_day)?
            .ok_or_else(|| eyre!("native Lysis fixture WorldwideDay is missing"))?;
    eyre::ensure!(
        projection.is_offering_at(timestamp),
        "native Lysis fixture WorldwideDay must be Offering"
    );
    let price_day = outbe_nod::openings::entry_price_day(projection.offering_end)
        .map_err(|error| eyre!("native Lysis entry-price date: {error}"))?;
    let last_closed_day = outbe_nod::openings::entry_price_day(timestamp)
        .map_err(|error| eyre!("native Lysis entry-price date: {error}"))?;
    eyre::ensure!(
        price_day <= last_closed_day,
        "native Lysis entry-price day must close before genesis"
    );
    eyre::ensure!(
        outbe_metadosis::api::emission_day(storage.clone(), projection.day)?.is_none(),
        "native Lysis fixture already has an emission allocation"
    );
    let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
    let finalized = oracle.utc_day_vwap_last_finalized.read()?;
    eyre::ensure!(
        finalized <= last_closed_day,
        "Oracle fixture must not finalize a day after genesis"
    );
    for &(iso, price) in prices {
        let index = oracle.pair_index_of(outbe_oracle::types::AddressPair::new_coen_to(iso))?;
        eyre::ensure!(
            index != 0,
            "native Lysis entry-price pair is not registered"
        );
        oracle.record_utc_day_vwap(price_day, index, price)?;
    }
    oracle
        .utc_day_vwap_last_finalized
        .write(finalized.max(price_day))?;
    for &(iso, price) in prices {
        let actual = outbe_oracle::api::get_utc_day_vwap_for_iso(storage.clone(), price_day, iso)?;
        eyre::ensure!(
            actual == Some(price),
            "native Lysis entry price differs from the fixture"
        );
    }
    let genesis_midnight = timestamp - timestamp % outbe_primitives::time::SECONDS_PER_DAY;
    let day_number = WorldwideDay::new(projection.day)
        .to_timestamp_utc()
        .checked_sub(genesis_midnight)
        .ok_or_else(|| eyre!("native Lysis emission day precedes genesis"))?
        / outbe_primitives::time::SECONDS_PER_DAY;
    let metadosis_limit_minor =
        outbe_primitives::emission::metadosis_day_emission_limit(u32::try_from(day_number)?);
    let metadosis_limit = u128::try_from(metadosis_limit_minor)
        .map_err(|_| eyre!("native Lysis metadosis limit overflows u128"))?;
    let record = outbe_metadosis::EmissionDay {
        day: projection.day,
        wwd: worldwide_day,
        metadosis_start: projection.offering_end,
        state: outbe_emission::schema::emission_day_state::PROCESSING,
        metadosis_state: outbe_metadosis::emission_day_state::NONE,
        emission_limit: metadosis_limit,
        metadosis_limit,
        lysis_limit: 0,
        desis_limit: 0,
        validators: 0,
        waa: 0,
        sra: 0,
        cca: 0,
        nods: 0,
        promis_limit: 0,
    };
    let binding = outbe_primitives::storage::metadosis_process_ready_binding(
        storage.chain_id()?,
        storage.block_number()?,
        timestamp,
    );
    storage.with_metadosis_mutation::<outbe_primitives::storage::MetadosisCycleLifecycle, _>(
        binding,
        || {
            outbe_primitives::storage::dsl::Map::<u32, outbe_metadosis::EmissionDay>::new(
                U256::ZERO,
                outbe_primitives::addresses::EMISSION_ADDRESS,
                storage.clone(),
            )
            .create(&record)
        },
    )?;
    let expected = outbe_metadosis::EmissionDayProjection::from(record);
    eyre::ensure!(
        outbe_metadosis::api::emission_day(storage, projection.day)? == Some(expected),
        "native Lysis fixture emission allocation differs after storage"
    );
    Ok(())
}

/// Keep this fixture's ordinary mineGratis path independent from the
/// stablecoin/vault scenarios. Together with the Tribute amount, this price
/// produces non-zero Gratis and zero mining cost.
fn write_measurement_vwaps(
    storage: StorageHandle<'_>,
    day: &MeasurementDay,
) -> PrecompileResult<()> {
    let pair = outbe_oracle::api::DAY_TYPE_PAIR;
    let current_vwap_minor = U256::from(2);
    let volume = U256::from(1_000_000_u64);
    let mut oracle = outbe_oracle::schema::OracleContract::new(storage);
    oracle.write_snapshot(day.forming_start, &[(pair, current_vwap_minor, volume)])?;
    let pair_index = oracle.pair_index_of(pair)?;
    if pair_index == 0 {
        return Err(fatal(
            "native Tribute fixture Oracle pair is not registered",
        ));
    }
    let vwap_day = day.worldwide_day;
    oracle.worldwide_day_vwap_exists.write(&vwap_day, true)?;
    oracle
        .worldwide_day_vwap_start
        .write(&vwap_day, day.forming_start)?;
    oracle
        .worldwide_day_vwap_end
        .write(&vwap_day, day.forming_end)?;
    oracle
        .worldwide_day_vwap_value
        .get_nested(&vwap_day)
        .write(&pair_index, current_vwap_minor)?;
    Ok(())
}

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
            .entry(account_key)
            .or_insert_with(|| serde_json::json!({"storage": {}, "balance": "0x0"}))
            .as_object_mut()
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

fn find_alloc_address_key(
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

fn parse_storage_word(value: &serde_json::Value) -> Result<U256> {
    let encoded = value
        .as_str()
        .ok_or_else(|| eyre::eyre!("genesis storage word is not a string"))?;
    parse_hex_word(encoded)
}

fn parse_hex_word(encoded: &str) -> Result<U256> {
    U256::from_str_radix(encoded.strip_prefix("0x").unwrap_or(encoded), 16).map_err(Into::into)
}

fn replace_json_atomically(path: &Path, value: &serde_json::Value) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| eyre::eyre!("generated genesis has no parent directory"))?;
    let temporary = parent.join(format!(".genesis.tribute.{}.tmp", std::process::id()));
    let bytes = serde_json::to_vec_pretty(value)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o640)
        .open(&temporary)?;
    if let Err(error) = (|| -> Result<()> {
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })() {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(())
}

pub(super) fn capacity_tribute_private_keys(count: usize) -> Result<Vec<String>> {
    const FIRST_CAPACITY_SCALAR: u64 = 0x1_0000;

    let mut private_keys = Vec::new();
    private_keys.try_reserve_exact(count)?;
    for index in 0..count {
        let scalar = FIRST_CAPACITY_SCALAR
            .checked_add(u64::try_from(index)?)
            .ok_or_else(|| eyre::eyre!("capacity Tribute owner scalar overflow"))?;
        let mut bytes = [0_u8; 32];
        bytes[24..].copy_from_slice(&scalar.to_be_bytes());
        k256::ecdsa::SigningKey::from_bytes((&bytes).into())
            .map_err(|error| eyre::eyre!("invalid capacity Tribute owner scalar: {error}"))?;
        private_keys.push(format!("0x{}", hex::encode(bytes)));
    }
    Ok(private_keys)
}

pub(super) fn fund_capacity_tribute_accounts(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_measurement_day_precedes_cycle_utc_day_at_date_boundaries() {
        for (utc_day, seconds, forming, expected) in [
            (20260101, 0, 180_000, 20251230),
            (20261017, 35_999, 180_000, 20261015),
            (20261017, 36_000, 180_000, 20261015),
            (20261017, 43_199, 180_000, 20261015),
            (20261017, 43_200, 180_000, 20261016),
            (20261017, 86_399, 180_000, 20261016),
            (20260301, 0, 180_000, 20260227),
            (20240301, 36_000, 180_000, 20240228),
            (20240301, 43_200, 180_000, 20240229),
            (20260101, 0, 60, 20251231),
            (20240301, 36_000, 60, 20240229),
        ] {
            let execution_day = WorldwideDay::new(utc_day);
            let timestamp = execution_day.to_timestamp_utc() + seconds;
            let day = MeasurementDay::new(timestamp, forming, 600).unwrap();
            assert_eq!(day.worldwide_day.value(), expected);
            assert!(day.worldwide_day < execution_day);
            assert_eq!(day.forming_start, day.worldwide_day.start_timestamp());
            assert_eq!(day.forming_end - day.forming_start, forming);
            assert!(day.forming_end <= timestamp);
            assert_eq!(day.genesis_timestamp, timestamp);
            assert_eq!(day.offering_end, timestamp + 600);
        }
        let parameters = outbe_chain_constants::GenesisProtocolParametersV1::from_genesis(
            &serde_json::json!({"config": {"outbeProtocol": {
                "schemaVersion": 1,
                "metadosis": {"lookbackDelaySeconds": 79_200}
            }}}),
        )
        .unwrap();
        let timestamp = WorldwideDay::new(20260101).to_timestamp_utc();
        let day = MeasurementDay::new(timestamp, parameters.metadosis_forming_period_seconds, 600)
            .unwrap();
        assert_eq!(day.worldwide_day.value(), 20251230);
        assert_eq!(day.forming_end - day.forming_start, 180_000);
    }

    #[test]
    fn native_lysis_fixture_roundtrip_has_canonical_window_and_linked_allocation() {
        use outbe_primitives::block::{BlockContext, BlockRuntimeContext};

        const CHAIN_ID: u64 = 54_322_345;
        const TIMESTAMP: u64 = 1_792_926_350;
        let mut genesis = serde_json::json!({
            "config": {"chainId": CHAIN_ID},
            "timestamp": format!("0x{TIMESTAMP:x}"),
            "alloc": {}
        });
        for address in [
            EMISSION_ADDRESS,
            METADOSIS_ADDRESS,
            ORACLE_ADDRESS,
            TRIBUTE_ADDRESS,
            TRIBUTE_FACTORY_ADDRESS,
        ] {
            genesis["alloc"][format!("{address:#x}")] =
                serde_json::json!({"storage": {}, "balance": "0x0"});
        }
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        let keys = load_measurement_accounts(&genesis, &mut provider).unwrap();
        StorageHandle::enter(&mut provider, |storage| -> PrecompileResult<()> {
            for iso in [840, 978] {
                outbe_oracle::api::register_pair(
                    storage.clone(),
                    outbe_oracle::types::AddressPair::new_coen_to(iso),
                )?;
            }
            outbe_oracle::api::set_exchange_rate(
                storage,
                Address::ZERO,
                outbe_oracle::api::DAY_TYPE_PAIR,
                outbe_oracle::api::RateObservation {
                    rate: U256::from(1_000_000),
                    block_number: 0,
                    timestamp: TIMESTAMP,
                },
            )
        })
        .unwrap();
        write_measurement_accounts(&mut genesis, &provider, keys).unwrap();
        let parameters =
            outbe_chain_constants::GenesisProtocolParametersV1::from_genesis(&genesis).unwrap();
        let (_, day) = schedule_public_measurement_day(
            &mut genesis,
            CHAIN_ID,
            parameters.metadosis_forming_period_seconds,
            3_600,
        )
        .unwrap();
        assert_eq!(day.worldwide_day.value(), 20261023);
        assert_eq!(day.forming_start, 1_792_663_200);
        assert_eq!(day.forming_end, 1_792_843_200);

        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        let keys = load_measurement_accounts(&genesis, &mut provider).unwrap();
        provider.set_block_number(1);
        provider.set_timestamp(U256::from(TIMESTAMP));
        provider.enable_metadosis_mutation_frames(
            outbe_primitives::storage::MetadosisMutationPurposeTag::CycleLifecycle,
            1,
        );
        StorageHandle::enter(&mut provider, |storage| -> Result<()> {
            eyre::ensure!(
                outbe_metadosis::api::emission_day(storage.clone(), 20261025)?.is_none(),
                "ordinary Tribute fixture must stay unallocated"
            );
            seed_native_lysis_fixture(
                storage,
                day.worldwide_day,
                TIMESTAMP,
                &[(840, U256::from(1_000_000)), (978, U256::from(250_000))],
            )
        })
        .unwrap();
        write_measurement_accounts(&mut genesis, &provider, keys).unwrap();

        let expected = outbe_metadosis::EmissionDayProjection {
            day: 20261025,
            emission_limit_minor: U256::from(225_485_783_040_000_u64),
            wwd: WorldwideDay::new(20261023),
            metadosis_start: 1_792_929_950,
            state: outbe_metadosis::emission_day_state::NONE,
        };
        for timestamp in [TIMESTAMP, day.offering_end] {
            let mut provider = HashMapStorageProvider::new(CHAIN_ID);
            load_measurement_accounts(&genesis, &mut provider).unwrap();
            provider.set_block_number(1);
            provider.set_timestamp(U256::from(timestamp));
            provider.enable_metadosis_mutation_frames(
                outbe_primitives::storage::MetadosisMutationPurposeTag::CycleLifecycle,
                1,
            );
            StorageHandle::enter(&mut provider, |storage| {
                assert_eq!(
                    outbe_metadosis::api::emission_day(storage.clone(), 20261025).unwrap(),
                    Some(expected)
                );
                let context = BlockRuntimeContext::new(
                    BlockContext::empty_for_tests(1, timestamp, CHAIN_ID),
                    storage.clone(),
                );
                outbe_metadosis::commands::advance_active_worldwide_days(&context).unwrap();
                let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
                assert_eq!(
                    oracle
                        .worldwide_day_vwap_start
                        .read(&day.worldwide_day)
                        .unwrap(),
                    1_792_663_200
                );
                assert_eq!(
                    oracle
                        .worldwide_day_vwap_end
                        .read(&day.worldwide_day)
                        .unwrap(),
                    1_792_843_200
                );
                assert_eq!(
                    outbe_oracle::api::get_utc_day_vwap_for_iso(storage.clone(), 20261024, 840)
                        .unwrap(),
                    Some(U256::from(1_000_000))
                );
                assert_eq!(
                    outbe_metadosis::api::emission_day(storage, 20261025).unwrap(),
                    Some(expected)
                );
            });
        }
    }
}
