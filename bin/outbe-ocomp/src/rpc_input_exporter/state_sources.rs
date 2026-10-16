//! Complete, bounded reads of encrypted Tribute inputs from finalized EVM state.

use outbe_tee::tribute_source::TributeSourceSubject;
use outbe_tribute::state_proof::TributeDayStateProof;
use outbe_tribute::state_proof::{TributeDayProofVerifier, TributeRecordStateProof};

use crate::input_inventory::{StateTributeProofArchive, StateTributeProofArchiveBuilder};

use super::{stage, PublicOcompRpcClientV1, RpcInputExporterErrorV1};

/// Emits physical day order. The inventory consumer sorts canonical identities
/// on disk before publishing input chunks. At most one source proof is retained.
pub fn read_state_tribute_day(
    rpc: &PublicOcompRpcClientV1,
    subject: TributeSourceSubject,
    accept: impl FnMut(TributeRecordStateProof) -> Result<(), RpcInputExporterErrorV1>,
    on_progress: &impl Fn(),
) -> Result<(), RpcInputExporterErrorV1> {
    let initial = rpc
        .tribute_day_state_proof(&subject, None)
        .map_err(|error| stage("read Tribute day length proof", error))?;
    read_day(rpc, subject, initial, accept, on_progress)
}

/// Retains the full authenticated day before publishing any immutable archive.
/// A complete archive with a missing or changed source is an error, not a rebuild.
pub fn archive_state_tribute_day(
    rpc: &PublicOcompRpcClientV1,
    root: &std::path::Path,
    subject: TributeSourceSubject,
    on_progress: &impl Fn(),
) -> Result<StateTributeProofArchive, RpcInputExporterErrorV1> {
    if let Some(archive) = StateTributeProofArchive::try_open(root, subject, on_progress)
        .map_err(|error| stage("reopen Tribute state proof archive", error))?
    {
        return Ok(archive);
    }
    let initial = rpc
        .tribute_day_state_proof(&subject, None)
        .map_err(|error| stage("read Tribute day length proof", error))?;
    let mut builder =
        StateTributeProofArchiveBuilder::create(root, subject, initial.day_index_proof.clone())
            .map_err(|error| stage("create Tribute state proof archive", error))?;
    read_day(
        rpc,
        subject,
        initial,
        |proof| {
            builder
                .push(&proof)
                .map_err(|error| stage("retain Tribute state proof", error))
        },
        on_progress,
    )?;
    builder
        .finish_observing(on_progress)
        .map_err(|error| stage("seal Tribute state proof archive", error))
}

fn read_day(
    rpc: &PublicOcompRpcClientV1,
    subject: TributeSourceSubject,
    initial: TributeDayStateProof,
    mut accept: impl FnMut(TributeRecordStateProof) -> Result<(), RpcInputExporterErrorV1>,
    on_progress: &impl Fn(),
) -> Result<(), RpcInputExporterErrorV1> {
    let limits = outbe_tee::tribute_source::proof_limits();
    if initial.record.is_some() {
        return Err(stage(
            "read Tribute day length proof",
            "unexpected source record",
        ));
    }
    let mut verifier =
        TributeDayProofVerifier::new(subject.into(), &initial.day_index_proof, limits)
            .map_err(|error| stage("verify Tribute day length", error))?;
    drop(initial);
    on_progress();
    for index in 0..subject.tribute_count {
        let response = rpc
            .tribute_day_state_proof(&subject, Some(index))
            .map_err(|error| stage("read Tribute physical day index", error))?;
        let proof = response
            .record
            .ok_or_else(|| stage("read Tribute physical day index", "missing source record"))?;
        verifier
            .verify_next(&proof, limits)
            .map_err(|error| stage("verify Tribute physical day index", error))?;
        accept(proof)?;
        on_progress();
    }
    verifier
        .finish()
        .map_err(|error| stage("close complete Tribute day", error))
}
