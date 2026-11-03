//! Typed fresh-devnet genesis adaptation for cross-crate test harnesses.

use outbe_primitives::time::{timestamp_to_date_key, WorldwideDay};
use outbe_primitives::{error::Result, storage::StorageHandle};
use outbe_tribute::TributeContract;

use crate::schema::TributeFactoryContract;

use super::{
    aggregate::ValidatedWwdAggregate, errors::WwdStorageCorruptionError,
    schema::WorldwideDay as WorldwideDayRecord,
};

/// One invariant-valid active WWD included in a fresh-devnet genesis artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GenesisWorldwideDay {
    pub worldwide_day: WorldwideDay,
    pub day: u32,
    pub forming_start: u64,
    pub forming_end: u64,
    pub offerings_start: u64,
    pub offering_end: u64,
}

impl GenesisWorldwideDay {
    fn validate(mut self) -> Result<Self> {
        if !self.worldwide_day.is_valid() {
            return Err(WwdStorageCorruptionError::GenesisWwdNotValidCalendarDay.into());
        }
        if self.offerings_start == 0
            || ![
                self.forming_start,
                self.forming_end,
                self.offerings_start,
                self.offering_end,
            ]
            .windows(2)
            .all(|pair| pair[0] <= pair[1])
        {
            return Err(WwdStorageCorruptionError::GenesisWwdPhaseOrderInvalid.into());
        }
        if self.day == 0 {
            self.day = timestamp_to_date_key(self.offering_end);
        }
        Ok(self)
    }
}

enum GenesisOperation {
    SeedActive(GenesisWorldwideDay),
    RetimeOffering {
        worldwide_day: WorldwideDay,
        offering_end: u64,
    },
    ClearSingleOffering {
        expected_worldwide_day: WorldwideDay,
    },
}

/// Ordered typed mutations applied while constructing a fresh-devnet genesis.
#[derive(Default)]
pub struct FreshDevnetGenesisBuilder {
    operations: Vec<GenesisOperation>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FreshDevnetGenesisReport {
    pub changed: bool,
}

impl FreshDevnetGenesisBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn seed_active_worldwide_day(mut self, day: GenesisWorldwideDay) -> Self {
        self.operations.push(GenesisOperation::SeedActive(day));
        self
    }

    #[must_use]
    pub fn retime_offering_day(mut self, worldwide_day: WorldwideDay, offering_end: u64) -> Self {
        self.operations.push(GenesisOperation::RetimeOffering {
            worldwide_day,
            offering_end,
        });
        self
    }

    #[must_use]
    pub fn clear_single_offering_day(mut self, expected_worldwide_day: WorldwideDay) -> Self {
        self.operations.push(GenesisOperation::ClearSingleOffering {
            expected_worldwide_day,
        });
        self
    }

    pub fn apply(self, storage: StorageHandle<'_>) -> Result<FreshDevnetGenesisReport> {
        if storage.block_number()? > 1 {
            return Err(
                WwdStorageCorruptionError::GenesisBuilderCannotRunAfterPreLaunchBoundary.into(),
            );
        }
        let mut changed = false;
        let contract = TributeFactoryContract::new(storage.clone());
        for operation in self.operations {
            let Some(operation_changed) = operation.apply(&contract)? else {
                continue;
            };
            changed |= operation_changed;
            ValidatedWwdAggregate::load_and_validate(storage.clone())?;
        }
        Ok(FreshDevnetGenesisReport { changed })
    }
}

impl GenesisOperation {
    fn apply(self, contract: &TributeFactoryContract<'_>) -> Result<Option<bool>> {
        match self {
            Self::SeedActive(day) => Self::seed(contract, day),
            Self::RetimeOffering {
                worldwide_day,
                offering_end,
            } => Self::retime(contract, worldwide_day, offering_end),
            Self::ClearSingleOffering {
                expected_worldwide_day,
            } => Self::clear(contract, expected_worldwide_day),
        }
    }

    fn seed(
        contract: &TributeFactoryContract<'_>,
        day: GenesisWorldwideDay,
    ) -> Result<Option<bool>> {
        let day = day.validate()?;
        if contract.worldwide_days.get(day.worldwide_day)?.is_some() {
            return Err(WwdStorageCorruptionError::GenesisWwdAlreadyExists.into());
        }
        contract.worldwide_days.create(&WorldwideDayRecord {
            wwd: day.worldwide_day,
            emission_day: day.day,
            offering_start: day.offerings_start,
            offering_end: day.offering_end,
        })?;
        contract.pipeline_wwd.insert(day.worldwide_day)?;
        let tribute = TributeContract::new(contract.storage.clone());
        tribute
            .day_emission_day
            .write(&day.worldwide_day, day.day)?;
        Ok(Some(true))
    }

    fn retime(
        contract: &TributeFactoryContract<'_>,
        worldwide_day: WorldwideDay,
        offering_end: u64,
    ) -> Result<Option<bool>> {
        let mut day = contract
            .worldwide_days
            .get(worldwide_day)?
            .ok_or(WwdStorageCorruptionError::GenesisOfferingWwdMissing)?;
        let forming_end = day.forming_end().map_err(super::errors::date_step_error)?;
        if forming_end > offering_end || day.offering_start > offering_end {
            return Err(WwdStorageCorruptionError::GenesisWwdPhasesEndAfterOfferingDeadline.into());
        }
        let operation_changed = day.offering_end != offering_end;
        day.offering_end = offering_end;
        contract.worldwide_days.update(&day)?;
        Ok(Some(operation_changed))
    }

    fn clear(
        contract: &TributeFactoryContract<'_>,
        expected_worldwide_day: WorldwideDay,
    ) -> Result<Option<bool>> {
        let pipeline = contract.pipeline_wwd.read_all()?;
        if pipeline.len() > 1
            || pipeline
                .first()
                .is_some_and(|day| *day != expected_worldwide_day)
        {
            return Err(WwdStorageCorruptionError::GenesisContainsUnexpectedActiveWwd.into());
        }
        if contract
            .worldwide_days
            .get(expected_worldwide_day)?
            .is_none()
        {
            if pipeline.is_empty() {
                return Ok(None);
            }
            return Err(WwdStorageCorruptionError::GenesisActiveWwdRecordMissing.into());
        }
        contract.pipeline_wwd.remove(&expected_worldwide_day)?;
        contract.worldwide_days.delete(expected_worldwide_day)?;
        Ok(Some(true))
    }
}

/// Registers a deterministic fixture L2 network and submits a signed `offer_tribute`
/// targeting `worldwide_day` for cross-crate lifecycle tests.
pub fn submit_signed_test_offer_for_day(
    storage: StorageHandle<'_>,
    worldwide_day: WorldwideDay,
) -> Result<()> {
    use alloy_primitives::{Address, Bytes, U256};
    use outbe_l2registry::L2RegistryContract;
    use outbe_zk::demo_tribute::COMBINED_LEN as DEMO_TRIBUTE_COMBINED_LEN;

    const L2_CHAIN_ID: u64 = 0xdead;
    const PUB_HEX: &str = concat!(
        "000000000000000000000000000000001829f9fd76d079565e7d8a6616bf247a87eceb4531477f0be2e0668479902f6ea5507432d979bcb0f431b1099108693d",
        "0000000000000000000000000000000011697208ae3a8d9be58fb66f912d87f462c5e9054176014a75b834bf42f1f3b2c19df137e4efcfd456d1899b7725bb5f",
        "0000000000000000000000000000000007fa4a9e26691e08d61632d4c7b5bd55fbe7baf448eeb79a70d1b8c0b98c9d81873b6a781402db0863c1c363aff7cbab",
        "0000000000000000000000000000000004facf29f7f5bd15edbe4ca219dc2c0c6d0ad35750842ede67cb83e275ca587ccdc470ae27db4a89f99737cfed8fb9c7"
    );
    const SIG_HEX: &str =
        "a38cbc31b858bc5f6d41adfa7f14be151cff370515dfc788568540e9ee3113a94547b318304270a23d2e4bb9bf247977";

    let caller = Address::repeat_byte(0x77);
    let pub_bytes = hex::decode(PUB_HEX).expect("valid fixture pubkey hex");
    let sig_bytes = hex::decode(SIG_HEX).expect("valid fixture sig hex");
    let root = [0x04u8; 32];

    L2RegistryContract::new(storage.clone())
        .register_network(L2_CHAIN_ID, caller, &pub_bytes)
        .expect("register fixture L2 network");

    let mut proof = Vec::with_capacity(DEMO_TRIBUTE_COMBINED_LEN);
    proof.extend_from_slice(&4u32.to_be_bytes());
    proof.extend_from_slice(&[0x01; 32]);
    proof.extend_from_slice(&[0x02; 32]);
    proof.extend_from_slice(&[0x03; 32]);
    proof.extend_from_slice(&root);
    proof.resize(DEMO_TRIBUTE_COMBINED_LEN, 0);

    let input = crate::runtime::OfferTributeInput {
        caller,
        cipher_text: Bytes::new(),
        nonce: Bytes::new(),
        ephemeral_pubkey: U256::ZERO,
        worldwide_day,
        tribute_currency: 840,
        reference_currency: 840,
        zk_proof: proof.into(),
        l2_chain_id: u32::try_from(L2_CHAIN_ID).expect("u32 chain id"),
        circuit_version: "1.1.0".to_owned(),
        zk_merkle_root: Bytes::copy_from_slice(&root),
        signature: sig_bytes.into(),
    };
    TributeFactoryContract::new(storage)
        .offer_tribute(input)
        .map(|_| ())
}
