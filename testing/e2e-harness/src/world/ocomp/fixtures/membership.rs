use crate::world::ocomp::*;
#[cfg(feature = "ocomp-integration")]
use outbe_primitives::time::SECONDS_PER_DAY;

impl OcompTopology {
    /// Stage the next validator's local runtime material before it starts in
    /// validator mode. This does not add it to the voting topology. Membership
    /// changes only after the canonical ValidatorSet activation boundary.
    #[cfg(feature = "ocomp-integration")]
    pub fn stage_joiner_domain_material(&self, validator_index: u8) -> Result<()> {
        let index = usize::from(validator_index);
        eyre::ensure!(
            index == self.domains.len(),
            "staged joiner must be the next ordered validator index"
        );
        let evm_key = format!("{}\n", ocomp_evm_private_key(validator_index));
        stage_joiner_domain_material_with_evm_key(&self.cfg, index, evm_key.as_bytes())
    }

    /// Prepare two independently scheduled public jobs around one real DKG
    /// membership boundary. The shortened epoch is still above the normative
    /// snapshot-retention lower bound. The compute-and-vote deadline comes from
    /// the test-only genesis override selected by the E2E node build.
    #[cfg(feature = "ocomp-integration")]
    pub fn prepare_dynamic_membership_fork_install(&self) -> Result<OcompDynamicMembershipForkV1> {
        let genesis_path = self.cfg.dir.join("genesis.json");
        let mut genesis: serde_json::Value = serde_json::from_slice(&fs::read(&genesis_path)?)?;
        let chain_id = genesis_chain_id(&genesis)?;
        let config = genesis
            .get("config")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| eyre::eyre!("generated genesis config is not an object"))?;
        eyre::ensure!(
            config
                .get(outbe_node::ocomp::fork::EPOCH_LENGTH_BLOCKS_GENESIS_KEY)
                .and_then(serde_json::Value::as_u64)
                == Some(OCOMP_TEST_EPOCH_LENGTH_BLOCKS)
                && config
                    .get("dkgPrepareWindowBlocks")
                    .and_then(serde_json::Value::as_u64)
                    == Some(OCOMP_DYNAMIC_DKG_PREPARE_WINDOW_BLOCKS)
                && config
                    .get(GENESIS_CONFIG_KEY)
                    .and_then(|value| value.pointer("/ocomp/computeVoteWindowBlocks"))
                    .and_then(serde_json::Value::as_u64)
                    == Some(OCOMP_DYNAMIC_VOTE_WINDOW_BLOCKS),
            "dynamic OCOMP epoch, DKG and vote windows must be configured before ValidatorSet genesis is seeded"
        );
        schedule_public_measurement_day(
            &mut genesis,
            chain_id,
            OCOMP_DYNAMIC_FIRST_OFFERING_AFTER_GENESIS_SECS,
        )?;
        let schedule = schedule_dynamic_membership_days(&mut genesis, chain_id)?;
        replace_json_atomically(&genesis_path, &genesis)?;

        let fork = self.prepare_measurement_fork_install_inner(None, &[], false, false)?;
        Ok(OcompDynamicMembershipForkV1 {
            fork,
            first_worldwide_day: schedule.0,
            second_worldwide_day: schedule.1,
            first_processing_time: schedule.2,
            second_processing_time: schedule.3,
        })
    }
}

#[cfg(feature = "ocomp-integration")]
const OCOMP_DYNAMIC_FIRST_OFFERING_AFTER_GENESIS_SECS: u64 = 180;

#[cfg(feature = "ocomp-integration")]
pub(crate) const OCOMP_TEST_EPOCH_LENGTH_BLOCKS: u64 = 300;

#[cfg(feature = "ocomp-integration")]
pub(crate) const OCOMP_DYNAMIC_DKG_PREPARE_WINDOW_BLOCKS: u64 = 10;

#[cfg(feature = "ocomp-integration")]
pub(crate) const OCOMP_DYNAMIC_VOTE_WINDOW_BLOCKS: u64 = OCOMP_TEST_EPOCH_LENGTH_BLOCKS * 3 / 2;

/// Exact two-job schedule plus immutable fork used by the dynamic-membership E2E.
#[cfg(feature = "ocomp-integration")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OcompDynamicMembershipForkV1 {
    pub fork: OcompMeasurementForkV1,
    pub first_worldwide_day: WorldwideDay,
    pub second_worldwide_day: WorldwideDay,
    pub first_processing_time: u64,
    pub second_processing_time: u64,
}

/// Stage the node-owned OCOMP domain for a joiner that will start directly in
/// Validator mode. Without an explicit OCOMP delegate, the validator EVM key
/// is the canonical carrier authority.
#[cfg(feature = "ocomp-integration")]
pub(crate) fn stage_direct_joiner_domain_material(cfg: &Config, index: usize) -> Result<()> {
    let prefixed = crate::internal::proc::read_evm_key(&cfg.validator_dir(index))?;
    let raw = prefixed
        .strip_prefix("0x")
        .ok_or_else(|| eyre::eyre!("validator EVM key is missing its canonical prefix"))?;
    eyre::ensure!(
        raw.len() == 64
            && raw
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "validator EVM key must be exactly 32 lowercase hex bytes"
    );
    let evm_key = format!("{raw}\n");
    stage_joiner_domain_material_with_evm_key(cfg, index, evm_key.as_bytes())
}

#[cfg(feature = "ocomp-integration")]
fn stage_joiner_domain_material_with_evm_key(
    cfg: &Config,
    index: usize,
    evm_key: &[u8],
) -> Result<()> {
    let founder = cfg.validator_dir(0).join("ocomp").join("domain-v1");
    let bundle = fs::read(founder.join("protocol-bundle-v1.ocb1"))?;
    let signing_key = fs::read(cfg.validator_dir(index).join("ocomp-key-v1.hex"))?;
    let root = cfg.validator_dir(index).join("ocomp").join("domain-v1");
    fs::create_dir_all(&root)?;
    publish_exact_file(&root.join("protocol-bundle-v1.ocb1"), &bundle, 0o640)?;
    publish_exact_file(&root.join("ocomp-key-v1.hex"), &signing_key, 0o600)?;
    publish_exact_file(&root.join("ocomp-evm-key.hex"), evm_key, 0o600)?;
    Ok(())
}

#[cfg(feature = "ocomp-integration")]
fn schedule_dynamic_membership_days(
    genesis: &mut serde_json::Value,
    chain_id: u64,
) -> Result<(WorldwideDay, WorldwideDay, u64, u64)> {
    let genesis_timestamp = genesis
        .get("timestamp")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| eyre::eyre!("generated genesis has no timestamp"))
        .and_then(|encoded| u64::try_from(parse_hex_word(encoded)?).map_err(Into::into))?;
    let first_processing_time = genesis_timestamp
        .checked_add(OCOMP_DYNAMIC_FIRST_OFFERING_AFTER_GENESIS_SECS)
        .ok_or_else(|| eyre::eyre!("first dynamic OCOMP processing time overflow"))?;
    // The scenario's controlled-time jump deliberately releases Job B after
    // the certified five-validator activation. A relative `+700s`
    // deadline raced the height-300 activation when SGX/admission work delayed
    // blocks, allowing the job to pin the historical four-member snapshot.
    let second_processing_time = genesis_timestamp
        .checked_div(SECONDS_PER_DAY)
        .and_then(|day| day.checked_add(2))
        .and_then(|day| day.checked_mul(SECONDS_PER_DAY))
        .and_then(|midnight| midnight.checked_add(1))
        .ok_or_else(|| eyre::eyre!("second dynamic OCOMP processing time overflow"))?;
    let first_worldwide_day = WorldwideDay::from_timestamp(genesis_timestamp);
    let second_worldwide_day = WorldwideDay::from_timestamp(
        first_worldwide_day
            .start_timestamp()
            .checked_add(SECONDS_PER_DAY)
            .ok_or_else(|| eyre::eyre!("second dynamic OCOMP WorldwideDay overflow"))?,
    );
    seed_followup_public_day(
        genesis,
        chain_id,
        &FollowupDays {
            first: first_worldwide_day,
            first_processing_time: Some(first_processing_time),
            second: second_worldwide_day,
            second_processing_time,
        },
    )?;

    Ok((
        first_worldwide_day,
        second_worldwide_day,
        first_processing_time,
        second_processing_time,
    ))
}

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::ocomp) fn schedule_public_recovery_day(
    genesis: &mut serde_json::Value,
    chain_id: u64,
    first_worldwide_day: WorldwideDay,
) -> Result<WorldwideDay> {
    let second_worldwide_day = WorldwideDay::from_timestamp(
        first_worldwide_day
            .start_timestamp()
            .checked_add(SECONDS_PER_DAY)
            .ok_or_else(|| eyre::eyre!("public recovery WorldwideDay overflow"))?,
    );
    let second_processing_time = second_worldwide_day
        .start_timestamp()
        .checked_add(OCOMP_PUBLIC_OFFERING_AFTER_GENESIS_SECS)
        .ok_or_else(|| eyre::eyre!("public recovery processing time overflow"))?;
    seed_followup_public_day(
        genesis,
        chain_id,
        &FollowupDays {
            first: first_worldwide_day,
            first_processing_time: None,
            second: second_worldwide_day,
            second_processing_time,
        },
    )?;
    Ok(second_worldwide_day)
}

/// The first public job's day and the follow-up day seeded after it.
#[cfg(feature = "ocomp-integration")]
struct FollowupDays {
    first: WorldwideDay,
    first_processing_time: Option<u64>,
    second: WorldwideDay,
    second_processing_time: u64,
}

#[cfg(feature = "ocomp-integration")]
fn fatal(message: &str) -> outbe_primitives::error::PrecompileError {
    outbe_primitives::error::PrecompileError::Fatal(message.into())
}

/// Seed only the chain-state prerequisites for a later independent public job.
/// This function constructs no Tribute, OCOMP FSM, JobIntent, export, vote,
/// result or Nod. Those remain observable production effects of the running
/// scenario.
#[cfg(feature = "ocomp-integration")]
fn seed_followup_public_day(
    genesis: &mut serde_json::Value,
    chain_id: u64,
    days: &FollowupDays,
) -> Result<()> {
    let mut provider = followup_storage_provider(genesis, chain_id)?;
    StorageHandle::enter(&mut provider, |storage| seed_followup_state(storage, days))?;
    write_followup_storage(genesis, &provider)
}

#[cfg(feature = "ocomp-integration")]
const FOLLOWUP_ACCOUNTS: [(Address, &str); 3] = [
    (METADOSIS_ADDRESS, "Metadosis"),
    (ORACLE_ADDRESS, "Oracle"),
    (TRIBUTE_ADDRESS, "Tribute"),
];

#[cfg(feature = "ocomp-integration")]
fn followup_storage_provider(
    genesis: &serde_json::Value,
    chain_id: u64,
) -> Result<HashMapStorageProvider> {
    let mut provider = HashMapStorageProvider::new(chain_id);
    let alloc = genesis
        .get("alloc")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| eyre::eyre!("generated genesis has no alloc object"))?;
    for (address, label) in FOLLOWUP_ACCOUNTS {
        let Some(account_key) = find_alloc_address_key(alloc, address)? else {
            if address == TRIBUTE_ADDRESS {
                continue;
            }
            eyre::bail!("generated genesis has no {label} account");
        };
        let words = alloc
            .get(&account_key)
            .and_then(|account| account.get("storage"))
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| eyre::eyre!("{label} genesis account has no storage object"))?;
        for (slot, value) in words {
            provider
                .storage
                .insert((address, parse_hex_word(slot)?), parse_storage_word(value)?);
        }
    }
    Ok(provider)
}

#[cfg(feature = "ocomp-integration")]
fn seed_followup_state(
    storage: StorageHandle<'_>,
    days: &FollowupDays,
) -> std::result::Result<(), outbe_primitives::error::PrecompileError> {
    let first = outbe_metadosis::api::worldwide_day(storage.clone(), days.first)?
        .ok_or_else(|| fatal("follow-up OCOMP genesis is missing its first WorldwideDay"))?;
    if first.status != WwdStatus::Offering {
        return Err(fatal(
            "follow-up OCOMP genesis requires an OFFERING first WorldwideDay",
        ));
    }
    if days.second_processing_time <= first.scheduled_process_time {
        return Err(fatal(
            "follow-up OCOMP processing time must follow the first job",
        ));
    }

    let mut builder = FreshDevnetGenesisBuilder::new();
    if let Some(processing_time) = days.first_processing_time {
        builder = builder.retime_offering_day(days.first, processing_time);
    }
    builder
        .seed_active_worldwide_day(GenesisWorldwideDay {
            worldwide_day: days.second,
            status: WwdStatus::Offering,
            day_type: first.day_type,
            forming_start: first.forming_start,
            forming_end: first.forming_end,
            lookback_end: first.lookback_end,
            offering_end: days.second_processing_time,
            scheduled_process_time: days.second_processing_time,
            metadosis_limit_minor: first.metadosis_limit_minor,
            previous_vwap: first.previous_vwap,
            current_vwap: first.current_vwap,
        })
        .apply(storage.clone())?;
    outbe_tribute::TributeContract::new(storage.clone()).unseal_day(days.second)?;
    seed_followup_oracle(storage, days)
}

#[cfg(feature = "ocomp-integration")]
fn seed_followup_oracle(
    storage: StorageHandle<'_>,
    days: &FollowupDays,
) -> std::result::Result<(), outbe_primitives::error::PrecompileError> {
    let pair = outbe_oracle::api::DAY_TYPE_PAIR;
    let price = outbe_oracle::api::day_type_pair_vwap(storage.clone(), days.first)?
        .filter(|price| !price.is_zero())
        .ok_or_else(|| fatal("follow-up OCOMP genesis is missing its first Oracle VWAP"))?;
    let snapshot_time = days.second.start_timestamp();
    let snapshot_end = snapshot_time
        .checked_add(outbe_chain_constants::DEFAULT_METADOSIS_FORMING_PERIOD_SECONDS)
        .ok_or_else(|| fatal("follow-up OCOMP Oracle window overflow"))?;
    let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
    oracle
        .config_vote_period
        .write(E2E_ORACLE_VOTE_PERIOD_BLOCKS)?;
    let pair_index = oracle.pair_index_of(pair)?;
    if pair_index == 0 {
        return Err(fatal("follow-up OCOMP Oracle pair is not registered"));
    }
    // Job A freezes its entry price from the previous UTC day. The
    // WorldwideDay VWAP below is a separate Oracle value.
    if let Some(first_processing_time) = days.first_processing_time {
        let entry_price_day = outbe_primitives::time::previous_date_key(
            outbe_primitives::time::timestamp_to_date_key(first_processing_time),
        );
        oracle.record_utc_day_vwap(entry_price_day, pair_index, price)?;
        let last_finalized = oracle.utc_day_vwap_last_finalized.read()?;
        oracle
            .utc_day_vwap_last_finalized
            .write(last_finalized.max(entry_price_day))?;
        if outbe_oracle::api::get_utc_day_vwap_for_iso(storage.clone(), entry_price_day, 840)?
            != Some(price)
        {
            return Err(fatal(
                "dynamic OCOMP genesis is missing its finalized entry price",
            ));
        }
    }
    oracle.worldwide_day_vwap_exists.write(&days.second, true)?;
    oracle
        .worldwide_day_vwap_start
        .write(&days.second, snapshot_time)?;
    oracle
        .worldwide_day_vwap_end
        .write(&days.second, snapshot_end)?;
    oracle
        .worldwide_day_vwap_value
        .get_nested(&days.second)
        .write(&pair_index, price)?;
    Ok(())
}

#[cfg(feature = "ocomp-integration")]
fn write_followup_storage(
    genesis: &mut serde_json::Value,
    provider: &HashMapStorageProvider,
) -> Result<()> {
    let alloc = genesis
        .get_mut("alloc")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| eyre::eyre!("generated genesis has no alloc object"))?;
    for (address, label) in FOLLOWUP_ACCOUNTS {
        let account_key = match find_alloc_address_key(alloc, address)? {
            Some(account_key) => account_key,
            None if address == TRIBUTE_ADDRESS => {
                let account_key = hex::encode(address.as_slice());
                alloc.insert(
                    account_key.clone(),
                    serde_json::json!({
                        "code": "0xef",
                        "balance": "0x0",
                        "storage": {},
                    }),
                );
                account_key
            }
            None => {
                eyre::bail!("generated genesis has no {label} account");
            }
        };
        let words = alloc
            .get_mut(&account_key)
            .and_then(serde_json::Value::as_object_mut)
            .and_then(|account| account.get_mut("storage"))
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| eyre::eyre!("{label} genesis account has no storage object"))?;
        for ((stored_address, slot), value) in &provider.storage {
            if *stored_address != address {
                continue;
            }
            let slot = format!("0x{slot:064x}");
            if value.is_zero() {
                words.remove(&slot);
            } else {
                words.insert(slot, serde_json::Value::String(format!("0x{value:064x}")));
            }
        }
    }
    Ok(())
}
