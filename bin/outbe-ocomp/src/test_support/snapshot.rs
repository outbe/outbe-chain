use crate::{
    cas::FilesystemCas,
    control::poc_schema_limits,
    input_artifacts::{
        poc_input_list_limits, publish_input_artifact_set, InputArtifactContents,
        InputArtifactIdentity,
    },
    input_ref_catalog::VerifiedInputChunkRefCatalog,
};
use alloy_primitives::B256;
use outbe_compressed_entities::{encode_tribute_v1, TributeBodyV1};
use outbe_ocomp_protocol::{
    input::{CheckpointIdentityV1, InputChunkKind, InputManifestV1},
    profile::ProtocolBundleV1,
    unit::PlanCommitmentV1,
    CasObjectRefV1,
};
use outbe_primitives::time::WorldwideDay;

pub struct FixturePlanInputs<'a> {
    pub bundle: &'a ProtocolBundleV1,
    pub job_id: B256,
    pub day: WorldwideDay,
    pub tributes: &'a [TributeBodyV1],
    pub fidelity_openings: Vec<outbe_ocomp_protocol::input::AuthenticatedOpeningV1>,
    pub oracle_opening: outbe_ocomp_protocol::input::AuthenticatedOpeningV1,
}
pub struct PublishedFixturePlan {
    pub plan: PlanCommitmentV1,
    pub plan_ref: CasObjectRefV1,
    pub manifest_ref: CasObjectRefV1,
}
pub fn publish_fixture_plan(
    cas: &FilesystemCas,
    input_ref_root: &std::path::Path,
    inputs: FixturePlanInputs<'_>,
) -> PublishedFixturePlan {
    let FixturePlanInputs {
        bundle,
        job_id,
        day,
        tributes,
        fidelity_openings,
        oracle_opening,
    } = inputs;
    let limits = poc_schema_limits();
    let list_limits = poc_input_list_limits();
    let published = publish_input_artifact_set(
        cas,
        input_ref_root,
        bundle,
        InputArtifactContents {
            identity: InputArtifactIdentity {
                job_id,
                attempt: 0,
                checkpoint: CheckpointIdentityV1 {
                    finalized_block_number: 90,
                    finalized_block_hash: B256::repeat_byte(0x31),
                    finalized_state_root: B256::repeat_byte(0x32),
                    finalized_ce_root: B256::repeat_byte(0x33),
                    ce_schema_version: 1,
                },
                wwd: day.value(),
                sealed_tribute_collection_key: B256::repeat_byte(0x34),
                sealed_tribute_collection_root: B256::repeat_byte(0x35),
            },
            canonical_tributes: tributes
                .iter()
                .map(|tribute| encode_tribute_v1(tribute).unwrap())
                .collect(),
            fidelity_openings,
            oracle_opening,
        },
        &limits,
        list_limits,
    )
    .unwrap();
    let manifest = InputManifestV1::decode_canonical(
        cas.read_verified(&published.manifest_ref).unwrap().bytes(),
        &limits,
    )
    .unwrap();
    let input_refs_for_plan = VerifiedInputChunkRefCatalog::open(
        input_ref_root,
        cas,
        &published.manifest_ref,
        limits,
        list_limits,
    )
    .unwrap();
    let all_input_refs = input_refs_for_plan
        .exact_cursor()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let tribute_refs = all_input_refs
        .iter()
        .filter(|reference| reference.kind == InputChunkKind::Tribute)
        .cloned()
        .collect::<Vec<_>>();
    drop(input_refs_for_plan);
    let manifest_ref = published.manifest_ref;

    let planner =
        crate::test_support::fixture_planner(bundle, job_id, &manifest_ref, &manifest, &limits);
    let plan = planner
        .commit_primary_catalog(tribute_refs.clone(), &limits)
        .unwrap();
    let plan_ref = cas
        .publish_bytes(&plan.encode_canonical_record(&limits).unwrap())
        .unwrap();

    PublishedFixturePlan {
        plan,
        plan_ref,
        manifest_ref,
    }
}
