//! Build synthetic input catalogs and faulted result artifacts.

use super::*;
use outbe_ocomp_protocol::{profile::ProtocolBundleV1, unit::PlanCommitmentV1, SchemaLimits};
use std::path::PathBuf;

struct FixtureOptions {
    substitute_bucket_spec: bool,
    corrupt_fidelity_root: bool,
    result_fault: ResultCatalogFault,
    tribute_count: u32,
}
struct FixtureSetup {
    limits: SchemaLimits,
    bundle: ProtocolBundleV1,
    pinned_bundle: PinnedProtocolBundle,
    bundle_hash: B256,
    job_id: B256,
    day: WorldwideDay,
    tributes: Vec<TributeBodyV1>,
    contributors_by_owner: Vec<ContributorActionV1>,
    nod_action_tributes: Vec<TributeBodyV1>,
}
impl FixtureSetup {
    fn new(options: &FixtureOptions) -> Self {
        let tribute_count = options.tribute_count;
        let result_fault = options.result_fault;
        let limits = poc_schema_limits();
        let bundle = fixture_support::protocol_bundle_fixture();
        let bundle_hash = bundle.protocol_bundle_hash(&limits).unwrap();
        let pinned_bundle = PinnedProtocolBundle::decode(
            &bundle.encode_canonical(&limits).unwrap(),
            bundle_hash,
            &limits,
        )
        .unwrap();
        let job_id = hash(0x30);
        let day = WorldwideDay::new(20_260_725);

        let tributes = fixture_support::tribute_population(day, tribute_count);
        let mut contributors_by_owner = fixture_support::contributor_population(&tributes);
        if result_fault == ResultCatalogFault::ContributorBoundary {
            contributors_by_owner.swap(255, 256);
        }
        let mut nod_action_tributes = tributes.clone();
        if result_fault == ResultCatalogFault::NodBoundary {
            nod_action_tributes.swap(255, 256);
        }
        Self {
            limits,
            bundle,
            pinned_bundle,
            bundle_hash,
            job_id,
            day,
            tributes,
            contributors_by_owner,
            nod_action_tributes,
        }
    }
}
struct PublishedPlan {
    directory: tempfile::TempDir,
    cas_root: PathBuf,
    input_ref_root: PathBuf,
    admission_root: PathBuf,
    cas: FilesystemCas,
    manifest_ref: CasObjectRefV1,
    plan: PlanCommitmentV1,
    plan_ref: CasObjectRefV1,
}
impl PublishedPlan {
    fn new(setup: &FixtureSetup, options: &FixtureOptions) -> Self {
        let limits = setup.limits;
        let list_limits = poc_input_list_limits();
        let bundle = &setup.bundle;
        let job_id = setup.job_id;
        let day = setup.day;
        let tributes = &setup.tributes;
        let corrupt_fidelity_root = options.corrupt_fidelity_root;
        let openings = fixture_support::fixture_openings(bundle, job_id, day, tributes, &limits);
        let finalized_state_root = hash(0x32);
        let directory = support::tempdir().unwrap();
        let cas_root = directory.path().join("cas");
        let input_ref_root = directory.path().join("input-refs");
        let admission_root = directory.path().join("admissions");
        let cas = FilesystemCas::open(&cas_root, CasWriterRole::Supervisor, CAS_LIMITS).unwrap();
        let published = publish_input_artifact_set(
            &cas,
            &input_ref_root,
            bundle,
            InputArtifactContents {
                identity: InputArtifactIdentity {
                    job_id,
                    attempt: 0,
                    checkpoint: CheckpointIdentityV1 {
                        finalized_block_number: 90,
                        finalized_block_hash: hash(0x31),
                        finalized_state_root,
                        finalized_ce_root: hash(0x33),
                        ce_schema_version: 1,
                    },
                    wwd: day.value(),
                    sealed_tribute_collection_key: hash(0x34),
                    sealed_tribute_collection_root: hash(0x35),
                },
                canonical_tributes: tributes
                    .iter()
                    .map(|tribute| encode_tribute_v1(tribute).unwrap())
                    .collect(),
                fidelity_openings: openings.fidelity,
                oracle_opening: openings.oracle,
            },
            &limits,
            list_limits,
        )
        .unwrap();
        let mut manifest = InputManifestV1::decode_canonical(
            cas.read_verified(&published.manifest_ref).unwrap().bytes(),
            &limits,
        )
        .unwrap();
        let input_refs_for_plan = VerifiedInputChunkRefCatalog::open(
            &input_ref_root,
            &cas,
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
        let (manifest_ref, selected_input_ref_root) = if corrupt_fidelity_root {
            manifest.fidelity_opening_root = hash(0xee);
            let mut manifest_ref = cas
                .publish_bytes(&manifest.encode_canonical(&limits).unwrap())
                .unwrap();
            manifest_ref.expected_ocb1_kind = Some(ObjectKind::InputManifestV1.tag());
            let selected_input_ref_root = directory.path().join("input-refs-corrupt-root");
            let mut catalog = VerifiedInputChunkRefCatalog::open(
                &selected_input_ref_root,
                &cas,
                &manifest_ref,
                limits,
                list_limits,
            )
            .unwrap();
            for reference in &all_input_refs {
                catalog.admit(reference).unwrap();
            }
            catalog.exact_cursor().unwrap().for_each(|item| {
                item.unwrap();
            });
            drop(catalog);
            (manifest_ref, selected_input_ref_root)
        } else {
            (published.manifest_ref, input_ref_root)
        };

        let planner =
            fixture_support::fixture_planner(bundle, job_id, &manifest_ref, &manifest, &limits);
        let plan = planner
            .commit_primary_catalog(tribute_refs.clone(), &limits)
            .unwrap();
        let plan_ref = cas
            .publish_bytes(&plan.encode_canonical_record(&limits).unwrap())
            .unwrap();

        Self {
            directory,
            cas_root,
            input_ref_root: selected_input_ref_root,
            admission_root,
            cas,
            manifest_ref,
            plan,
            plan_ref,
        }
    }
}
struct ResultFixture<'a> {
    setup: &'a FixtureSetup,
    published: &'a PublishedPlan,
    options: &'a FixtureOptions,
}
struct AdmittedResults {
    target_ordinal: u32,
    expected_count: u32,
    result_chunk_refs: Vec<CasObjectRefV1>,
}
struct RootLeafFixture {
    artifact: UnitArtifactV1,
    entry: OutputManifestEntryV1,
    chunk_ref: CasObjectRefV1,
}
impl ResultFixture<'_> {
    fn admit(&self) -> AdmittedResults {
        let results = self;
        let limits = self.setup.limits;
        let list_limits = poc_input_list_limits();
        let pinned_bundle = &self.setup.pinned_bundle;
        let PublishedPlan {
            cas_root,
            input_ref_root: selected_input_ref_root,
            admission_root,
            cas,
            manifest_ref,
            plan,
            plan_ref,
            ..
        } = self.published;
        let substitute_bucket_spec = self.options.substitute_bucket_spec;
        let reader = FilesystemCasReader::open(cas_root, CAS_LIMITS).unwrap();
        let input_refs = VerifiedInputChunkRefCatalog::reopen(
            selected_input_ref_root,
            &reader,
            limits,
            list_limits,
        )
        .unwrap();
        let mut admissions =
            VerifiedAdmissionCatalog::open(admission_root, cas, plan_ref, manifest_ref, limits)
                .unwrap();
        let topology = LysisPlanTopologyV1::new(plan.primary_work_unit_count).unwrap();
        let target_ordinal = topology.phase_offset(UnitPhase::BucketShuffle).unwrap()
            + topology.phase_unit_count(UnitPhase::BucketShuffle)
            - 1;
        let plan_hash = plan.plan_hash(&limits).unwrap();
        let mut result_chunk_refs = Vec::new();

        for plan_ordinal in 0..topology.total_unit_count() {
            let mut spec = {
                let audit = LocalLysisPlanAuditV1::open(
                    &admissions,
                    &input_refs,
                    &reader,
                    pinned_bundle,
                    &limits,
                )
                .unwrap();
                audit.candidate_spec_at(plan_ordinal).unwrap()
            };
            if substitute_bucket_spec && plan_ordinal == target_ordinal {
                let producer = spec
                    .canonical_ordered_inputs
                    .get_mut(1)
                    .expect("merged Bucket spec has producer input");
                producer.source_id = hash(0xf1);
                spec.validate_semantics(&limits).unwrap();
            }
            let (artifact, result_entry) = match topology.plan_position_at(plan_ordinal).unwrap() {
                PlannedUnitPositionV1::TreeNode {
                    phase: UnitPhase::RootReduce,
                    level: 0,
                    index,
                } => {
                    let leaf = results.root_leaf(index, plan_hash, &spec);
                    result_chunk_refs.push(leaf.chunk_ref);
                    (leaf.artifact, Some(leaf.entry))
                }
                _ => (
                    UnitArtifactV1::from_canonical_output(
                        &spec,
                        WorkOutputHeaderV1 {
                            source_coverage_root: hash(0xa1),
                            output_coverage_root: hash(0xa2),
                            source_coverage_count: 1,
                            output_coverage_count: 1,
                        },
                        BoundedBytes(vec![0x42]),
                        &limits,
                    )
                    .unwrap(),
                    None,
                ),
            };
            let mut artifact_ref = cas
                .publish_bytes(&artifact.encode_canonical(&limits).unwrap())
                .unwrap();
            artifact_ref.expected_ocb1_kind = Some(ObjectKind::UnitArtifactV1.tag());
            admissions
                .admit_verified_unit(
                    AdmissionPositionV1 { plan_ordinal },
                    &spec,
                    artifact_ref,
                    result_entry,
                )
                .unwrap();
        }
        drop(admissions);
        drop(input_refs);
        drop(reader);
        AdmittedResults {
            target_ordinal,
            expected_count: topology.total_unit_count(),
            result_chunk_refs,
        }
    }
    fn root_leaf(&self, index: u32, plan_hash: B256, spec: &UnitSpecV1) -> RootLeafFixture {
        let limits = self.setup.limits;
        let bundle_hash = self.setup.bundle_hash;
        let job_id = self.setup.job_id;
        let tributes = &self.setup.tributes;
        let contributors_by_owner = &self.setup.contributors_by_owner;
        let nod_action_tributes = &self.setup.nod_action_tributes;
        let result_fault = self.options.result_fault;
        let cas = &self.published.cas;
        let start = usize::try_from(index * 256).unwrap();
        let end = (start + 256).min(tributes.len());
        let mut actions = fixture_support::nod_actions(
            &nod_action_tributes[start..end],
            u32::try_from(start).unwrap(),
        );
        if result_fault == ResultCatalogFault::AlternateResultChunk && index == 0 {
            actions[0].settlement_cost_minor = U256::from(3);
        }
        let contributors = contributors_by_owner[start..end].to_vec();
        let chunk = ResultChunkV1 {
            protocol_bundle_hash: bundle_hash,
            job_id,
            attempt: 0,
            chunk_ordinal: index,
            first_nod_ordinal: u32::try_from(start).unwrap(),
            ordered_nod_actions: actions.clone(),
            ordered_eligible_contributors: contributors.clone(),
        };
        let chunk_hash = chunk.result_chunk_hash(&limits).unwrap();
        let mut chunk_ref = cas
            .publish_bytes(&chunk.encode_canonical(&limits).unwrap())
            .unwrap();
        chunk_ref.expected_ocb1_kind = Some(ObjectKind::ResultChunkV1.tag());
        let entry = OutputManifestEntryV1 {
            chunk_ordinal: index,
            result_chunk_hash: chunk_hash,
            result_chunk_ref: chunk_ref.clone(),
        };
        let mut summary = fixture_support::root_summary_fixture(
            plan_hash,
            &chunk,
            &tributes[start..end],
            &entry,
            &limits,
        );
        if index == 1 {
            match result_fault {
                ResultCatalogFault::ContributorCarrier => {
                    summary.contributor_actions.tree_root = hash(0xed);
                }
                ResultCatalogFault::EligibleTotal => {
                    summary.eligible_nominal_total -= U256::from(1);
                }
                ResultCatalogFault::None
                | ResultCatalogFault::HeaderCoverage
                | ResultCatalogFault::ContributorBoundary
                | ResultCatalogFault::NodBoundary
                | ResultCatalogFault::AlternateResultChunk => {}
            }
        }
        let coverage_root = summary.result_chunk_hashes.tree_root;
        let output_coverage_root =
            if result_fault == ResultCatalogFault::HeaderCoverage && index == 1 {
                hash(0xec)
            } else {
                coverage_root
            };
        let artifact = fixture_support::root_leaf_artifact(
            spec,
            summary,
            &entry,
            output_coverage_root,
            &limits,
        );
        RootLeafFixture {
            artifact,
            entry,
            chunk_ref,
        }
    }
}

pub(super) fn synthetic_fixture_with_options(
    substitute_bucket_spec: bool,
    corrupt_fidelity_root: bool,
    result_fault: ResultCatalogFault,
    tribute_count: u32,
) -> Fixture {
    let options = FixtureOptions {
        substitute_bucket_spec,
        corrupt_fidelity_root,
        result_fault,
        tribute_count,
    };
    let setup = FixtureSetup::new(&options);
    let published = PublishedPlan::new(&setup, &options);
    let admitted = ResultFixture {
        setup: &setup,
        published: &published,
        options: &options,
    }
    .admit();
    drop(published.cas);
    Fixture {
        _directory: published.directory,
        cas_root: published.cas_root,
        input_ref_root: published.input_ref_root,
        admission_root: published.admission_root,
        limits: setup.limits,
        bundle: setup.pinned_bundle,
        target_ordinal: admitted.target_ordinal,
        expected_count: admitted.expected_count,
        result_chunk_refs: admitted.result_chunk_refs,
    }
}
