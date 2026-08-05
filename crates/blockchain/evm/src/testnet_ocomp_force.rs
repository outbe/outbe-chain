//! TESTNET-ONLY forced OCOMP arming for chain 54322345 (v0.3.1-testnet.3).
//!
//! Chain 54322345 launched without the protocol-v1 update ever being proposed,
//! so `ocomp_profile_ready` stayed `false` in Tribute, Fidelity and Oracle and
//! every Metadosis terminal request deferred with `TributeProfileNotReady` —
//! silently, forever.
//!
//! Governance cannot repair it after the fact: the scheduled activation runs
//! the registered protocol-v1 handler, whose Tribute and Fidelity
//! preconditions reject a chain that already carries live supply and abort the
//! activating block with a `Fatal`. So the arming happens outside the Update
//! machinery, at one fixed height, identically on proposer and validators.
//!
//! The two knobs below are the whole operator surface. Delete this module,
//! its `mod` declaration in `lib.rs` and its call site in `executor.rs`
//! together with the chain they target.

use outbe_common::WorldwideDay;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::Result;

/// Chain id this forced arming applies to. Every other chain is untouched.
pub const FORCE_CHAIN_ID: u64 = 54_322_345;

/// Block at which every node of [`FORCE_CHAIN_ID`] arms the OCOMP profiles.
///
/// Must be far enough ahead that all validators run the new binary before the
/// block is proposed: a node still on the old binary produces a different
/// post-state at this height. Chain 54322345 runs at ~2.14 s/block.
pub const FORCE_HEIGHT: u64 = 221_200;

/// Arms the OCOMP profiles that protocol version 1 was supposed to arm.
///
/// No-op on every chain except [`FORCE_CHAIN_ID`] and every height except
/// [`FORCE_HEIGHT`]. Does not set an active protocol version: nothing in the
/// OCOMP path reads one.
pub fn force_ocomp_profile(ctx: &BlockRuntimeContext) -> Result<()> {
    if ctx.block.block_number != FORCE_HEIGHT || ctx.storage.chain_id()? != FORCE_CHAIN_ID {
        return Ok(());
    }

    ctx.with_checkpoint(|| {
        let mut tribute = ctx.contract::<outbe_tribute::TributeContract>();
        tribute.force_ocomp_profile_ready()?;

        let mut fidelity = ctx.contract::<outbe_fidelity::FidelityContract>();
        fidelity.force_ocomp_profile_ready()?;

        outbe_oracle::api::initialize_fresh_ocomp_profile(ctx.storage.clone())?;

        let wwd = WorldwideDay::from_timestamp(ctx.block.timestamp);
        outbe_metadosis::initialize_fresh_ocomp_profile(ctx.storage.clone(), wwd)?;
        Ok(())
    })
}
