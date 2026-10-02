//! Shared owner-bound pledge pool. Backing never enters Paynote's ERC20 vault.
use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::{sol, SolEvent};
use outbe_macros::{contract, storage_schema};
use outbe_primitives::{
    addresses::GRATIS_FACTORY_ADDRESS,
    error::{PrecompileError, Result},
    storage::StorageHandle,
};
use outbe_protocol::codec;
use outbe_zk_backend::barretenberg::verify_circuit;
use outbe_zk_canonical::{
    noir::{pledgenote_issue, pledgenote_unpledge},
    pledgenote as hash,
};

pub const DEPTH: usize = 32;
const CAPACITY: u64 = 1 << DEPTH;
#[storage_schema]
#[contract(addr = GRATIS_FACTORY_ADDRESS)]
pub struct PledgePool {
    #[attribute(order = 0)]
    pub current_root: outbe_primitives::storage::dsl::Value<B256>,
    #[attribute(order = 1)]
    pub leaf_count: outbe_primitives::storage::dsl::Value<u64>,
    #[attribute(order = 2)]
    pub filled_subtrees: outbe_primitives::storage::dsl::Map<u8, B256>,
    #[attribute(order = 3)]
    pub recent_roots: outbe_primitives::storage::dsl::CircularBuffer<B256>,
    #[attribute(order = 4)]
    pub commitments: outbe_primitives::storage::dsl::Map<B256, bool>,
    #[attribute(order = 5)]
    pub spent_nullifiers: outbe_primitives::storage::dsl::Map<B256, bool>,
}
sol!("../../../contracts/precompiles/src/IGratisFactory.sol");
pub use IGratisFactory::{PledgeNote, PledgeSpent};
fn invalid(error: impl std::fmt::Display) -> PrecompileError {
    PrecompileError::Revert(format!("pledge: {error}"))
}
fn field(word: B256) -> Result<hash::Field> {
    codec::field_from_b256(&word).map_err(invalid)
}
fn word(value: hash::Field) -> Result<B256> {
    codec::field_to_b256(&value).map_err(invalid)
}

/// Append a funded note or circuit-authenticated change. Called inside the economic checkpoint.
pub(crate) fn append(storage: &StorageHandle<'_>, commitment: B256, amount: U256) -> Result<()> {
    let pool = PledgePool::new(storage.clone());
    let index = pool.leaf_count.read()?;
    if index >= CAPACITY || commitment.is_zero() || pool.commitments.read(&commitment)? {
        return Err(invalid("invalid, duplicate or excess commitment"));
    }
    let zeros = hash::empty_subtrees(storage.chain_id()?, DEPTH).map_err(invalid)?;
    if index == 0 {
        pool.recent_roots.setup(32)?;
        pool.recent_roots.push(word(zeros[DEPTH])?)?;
    }
    let mut current = field(commitment)?;
    for (level, zero) in zeros.iter().enumerate().take(DEPTH) {
        let level = u8::try_from(level).map_err(invalid)?;
        let left = if (index >> level) & 1 == 0 {
            pool.filled_subtrees.write(&level, word(current)?)?;
            let next = hash::merkle_node(current, *zero).map_err(invalid)?;
            current = next;
            continue;
        } else {
            field(pool.filled_subtrees.read(&level)?)?
        };
        current = hash::merkle_node(left, current).map_err(invalid)?;
    }
    let root = word(current)?;
    pool.leaf_count.write(index + 1)?;
    pool.current_root.write(root)?;
    pool.recent_roots.push(root)?;
    pool.commitments.write(&commitment, true)?;
    storage.emit_event(
        GRATIS_FACTORY_ADDRESS,
        PledgeNote::encode_log_data(&PledgeNote {
            commitment,
            leafIndex: u32::try_from(index).map_err(invalid)?,
            rootAfter: root,
            amount,
        }),
    )
}

pub(crate) fn fund(
    storage: &StorageHandle<'_>,
    serial: B256,
    amount: U256,
    receipt: B256,
) -> Result<B256> {
    if amount.is_zero() || serial.is_zero() {
        return Err(invalid("zero funded note"));
    }
    let commitment = word(
        hash::note_commitment(storage.chain_id()?, field(serial)?, amount, field(receipt)?)
            .map_err(invalid)?,
    )?;
    append(storage, commitment, amount)?;
    Ok(commitment)
}

fn check(
    storage: &StorageHandle<'_>,
    chain_id: u64,
    root: B256,
    nullifier: B256,
    context: B256,
    amount: U256,
) -> Result<()> {
    let pool = PledgePool::new(storage.clone());
    if chain_id != storage.chain_id()?
        || amount.is_zero()
        || context.is_zero()
        || nullifier.is_zero()
    {
        return Err(invalid("invalid claim"));
    }
    if pool.leaf_count.read()? == 0 || !pool.recent_roots.read_all()?.contains(&root) {
        return Err(invalid("unknown root"));
    }
    if pool.spent_nullifiers.read(&nullifier)? {
        return Err(invalid("nullifier spent"));
    }
    Ok(())
}
fn book(storage: &StorageHandle<'_>, nullifier: B256, change: B256) -> Result<()> {
    PledgePool::new(storage.clone())
        .spent_nullifiers
        .write(&nullifier, true)?;
    if !change.is_zero() {
        append(storage, change, U256::ZERO)?;
    }
    storage.emit_event(
        GRATIS_FACTORY_ADDRESS,
        PledgeSpent::encode_log_data(&PledgeSpent { nullifier }),
    )
}
/// One rollback boundary must surround consumption and the consuming product's effects.
pub fn consume_issue(
    storage: &StorageHandle<'_>,
    proof: &[u8],
) -> Result<pledgenote_issue::alloy::PublicInputs> {
    storage.with_checkpoint(|| {
        let claim: pledgenote_issue::alloy::PublicInputs =
            pledgenote_issue::decode_public_inputs(proof)
                .and_then(TryInto::try_into)
                .map_err(invalid)?;
        check(
            storage,
            claim.chain_id,
            claim.root,
            claim.nullifier,
            claim.context,
            claim.spend_amount,
        )?;
        if claim.return_note_serial.is_zero()
            || !verify_circuit::<pledgenote_issue::PledgenoteIssue>(proof).map_err(invalid)?
        {
            return Err(invalid("invalid issue proof"));
        }
        book(storage, claim.nullifier, claim.change_commitment)?;
        Ok(claim)
    })
}
pub fn consume_unpledge(
    storage: &StorageHandle<'_>,
    proof: &[u8],
) -> Result<pledgenote_unpledge::alloy::PublicInputs> {
    storage.with_checkpoint(|| {
        let claim: pledgenote_unpledge::alloy::PublicInputs =
            pledgenote_unpledge::decode_public_inputs(proof)
                .and_then(TryInto::try_into)
                .map_err(invalid)?;
        check(
            storage,
            claim.chain_id,
            claim.root,
            claim.nullifier,
            claim.context,
            claim.spend_amount,
        )?;
        if claim.owner == Address::ZERO
            || !verify_circuit::<pledgenote_unpledge::PledgenoteUnpledge>(proof).map_err(invalid)?
        {
            return Err(invalid("invalid unpledge proof"));
        }
        book(storage, claim.nullifier, claim.change_commitment)?;
        Ok(claim)
    })
}
