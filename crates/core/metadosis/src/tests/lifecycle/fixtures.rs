use super::*;

pub(super) fn create_waiting_day(
    storage: &StorageHandle,
    wwd: outbe_primitives::time::WorldwideDay,
    dtype: u8,
    day_limit: U256,
) -> u64 {
    let mut metadosis = MetadosisContract::new(storage.clone());
    metadosis
        .create_worldwide_day(
            wwd,
            wwd.start_timestamp(),
            LOOKBACK_DELAY_HOURS,
            OFFERING_PERIOD_HOURS,
        )
        .unwrap();
    metadosis.add_active_wwd(wwd).unwrap();
    metadosis
        .set_wwd_day_type(wwd, WwdDayType::try_from(dtype).unwrap())
        .unwrap();
    metadosis
        .fixture_set_wwd_status(wwd, WwdStatus::Waiting)
        .unwrap();
    metadosis.set_metadosis_limit(wwd, day_limit).unwrap();
    metadosis
        .worldwide_days
        .entry(wwd)
        .scheduled_process_time()
        .read()
        .unwrap()
}

pub(super) struct FixtureTribute {
    pub(super) owner: Address,
    pub(super) wwd: outbe_primitives::time::WorldwideDay,
    pub(super) nominal: U256,
}

pub(super) fn issue_one_tribute(storage: &StorageHandle, input: FixtureTribute) {
    let FixtureTribute {
        owner,
        wwd,
        nominal,
    } = input;
    let mut tribute = TributeContract::new(storage.clone());
    crate::tests::tribute_fixture::issue_sealed_tribute(
        &mut tribute,
        crate::tests::tribute_fixture::FixtureTribute {
            owner,
            wwd,
            nominal,
        },
    );
}

pub(super) fn assert_no_ocomp_job(
    storage: &StorageHandle,
    wwd: outbe_primitives::time::WorldwideDay,
) {
    let metadosis = MetadosisContract::new(storage.clone());
    let limits = crate::ocomp::schema::poc_schema_limits();
    assert!(metadosis.ocomp_scheduler.is_empty().unwrap());
    assert!(metadosis.ocomp_ready_index.is_empty().unwrap());
    assert!(metadosis
        .ocomp_fsm_states
        .get_bytes(&wwd)
        .is_empty()
        .unwrap());
    assert_eq!(metadosis.terminal_intent_count(wwd).unwrap(), 0);
    assert!(metadosis
        .request_limit_receipt(wwd, &limits)
        .unwrap()
        .is_none());
    assert!(metadosis
        .read_pre_admission_envelope(wwd, &limits)
        .unwrap()
        .is_none());
}

pub(super) fn seed_missed_offering_day(
    provider: &mut HashMapStorageProvider,
    wwd: outbe_primitives::time::WorldwideDay,
    base_limit: U256,
    formation_carry_over: U256,
    later_carry_over: U256,
) -> u64 {
    provider.enable_metadosis_mutation_frame(
        outbe_primitives::storage::MetadosisMutationPurposeTag::CycleLifecycle,
    );
    StorageHandle::enter(provider, |storage| {
        arm_genesis_ocomp(&storage, CHAIN_ID);
        PromisLimitContract::new(storage.clone())
            .checked_add_carry_over(formation_carry_over)
            .unwrap();
        let formation_ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(
                1,
                wwd.start_timestamp() + 2 * SECONDS_PER_HOUR,
                CHAIN_ID,
            ),
            storage.clone(),
        );
        crate::emission_sink::apply(&formation_ctx, base_limit).unwrap();
        PromisLimitContract::new(storage.clone())
            .checked_add_carry_over(later_carry_over)
            .unwrap();
        MetadosisContract::new(storage)
            .worldwide_days
            .entry(wwd)
            .offering_end()
            .read()
            .unwrap()
    })
}

pub(super) fn run_start_command(
    provider: &mut HashMapStorageProvider,
    block_number: u64,
    timestamp: u64,
) -> outbe_primitives::error::Result<()> {
    provider.set_block_number(block_number);
    provider.set_timestamp(U256::from(timestamp));
    provider.enable_metadosis_mutation_frame(
        outbe_primitives::storage::MetadosisMutationPurposeTag::CycleLifecycle,
    );
    StorageHandle::enter(provider, |storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(block_number, timestamp, CHAIN_ID),
            storage,
        );
        crate::commands::start_metadosis(&ctx)
    })
}
