//! Build the authenticated input, plan, and admitted result fixtures.

use outbe_ocomp::test_support as fixture_support;

use super::*;
use outbe_ocomp_protocol::{
    unit::{PlanCommitmentV1, UnitSpecV1},
    SchemaLimits,
};

struct FixtureSetup {
    job_id: B256,
    day: WorldwideDay,
    tribute_count: u32,
    limits: SchemaLimits,
    bundle: ProtocolBundleV1,
    bundle_hash: B256,
    pinned_bundle: PinnedProtocolBundle,
}
impl FixtureSetup {
    fn new(job_seed: u8, day: WorldwideDay, tribute_count: u32) -> Self {
        let limits = poc_schema_limits();
        let bundle = protocol_bundle();
        let bundle_hash = bundle.protocol_bundle_hash(&limits).unwrap();
        let pinned_bundle = PinnedProtocolBundle::decode(
            &bundle.encode_canonical(&limits).unwrap(),
            bundle_hash,
            &limits,
        )
        .unwrap();

        Self {
            job_id: hash(job_seed),
            day,
            tribute_count,
            limits,
            bundle,
            bundle_hash,
            pinned_bundle,
        }
    }
}

struct SourcePopulation {
    tributes: Vec<TributeBodyV1>,
    contributors_by_owner: Vec<ContributorActionV1>,
    protected_sources: outbe_nodfactory::test_support::MaterializationFixture,
}
impl SourcePopulation {
    fn new(root: &Path, setup: &FixtureSetup) -> Self {
        let day = setup.day;
        let job_id = setup.job_id;
        let tribute_count = setup.tribute_count;
        let tributes = fixture_support::tribute_population(day, tribute_count);
        let contributors_by_owner = fixture_support::contributor_population(&tributes);
        let source_actions = fixture_support::nod_actions(&tributes, 0);
        let protected_sources =
            outbe_nodfactory::test_support::MaterializationFixture::new_with_archive(
                &source_actions,
                copied_native::chain().chain().id(),
                &root
                    .join("exporter-v1/input-refs/.work")
                    .join(hex::encode(job_id))
                    .join("inventory/source-proof-archive-v1"),
            )
            .unwrap();
        Self {
            tributes,
            contributors_by_owner,
            protected_sources,
        }
    }
    fn input_contents(&self, setup: &FixtureSetup) -> InputArtifactContents {
        let limits = setup.limits;
        let bundle = &setup.bundle;
        let job_id = setup.job_id;
        let day = setup.day;
        let tributes = &self.tributes;
        let protected_sources = &self.protected_sources;
        let openings = fixture_support::fixture_openings(bundle, job_id, day, tributes, &limits);
        let finalized_state_root = hash(0x32);
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
                sealed_tribute_collection_root: protected_sources.source_root(),
            },
            canonical_tributes: protected_sources.canonical_source_bodies().unwrap(),
            fidelity_openings: openings.fidelity,
            oracle_opening: openings.oracle,
        }
    }
}

struct PublishedPlan {
    cas: FilesystemCas,
    cas_root: PathBuf,
    input_ref_root: PathBuf,
    admission_root: PathBuf,
    manifest_ref: CasObjectRefV1,
    plan_ref: CasObjectRefV1,
    plan: PlanCommitmentV1,
}
impl PublishedPlan {
    fn new(root: &Path, setup: &FixtureSetup, population: &SourcePopulation) -> Self {
        let limits = setup.limits;
        let list_limits = poc_input_list_limits();
        let bundle = &setup.bundle;
        let bundle_hash = setup.bundle_hash;
        let job_id = setup.job_id;
        let job = hex::encode(job_id);
        let cas_root = root.join("cas-v1");
        let input_ref_root = root.join("exporter-v1/input-refs").join(&job);
        let admission_root = root
            .join("supervisor-v1/jobs")
            .join(&job)
            .join("admissions");
        let bundle_dir = root.join("protocol-bundles-v1");
        fs::create_dir_all(&bundle_dir).unwrap();
        fs::write(
            bundle_dir.join(format!("{}.ocb1", hex::encode(bundle_hash))),
            bundle.encode_canonical(&limits).unwrap(),
        )
        .unwrap();
        let cas = FilesystemCas::open(&cas_root, CasWriterRole::Supervisor, CAS_LIMITS).unwrap();
        let published = publish_input_artifact_set(
            &cas,
            &input_ref_root,
            bundle,
            population.input_contents(setup),
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
        let manifest_ref = published.manifest_ref;

        let planner =
            fixture_support::fixture_planner(bundle, job_id, &manifest_ref, &manifest, &limits);
        let plan = planner
            .commit_primary_catalog(tribute_refs.clone(), &limits)
            .unwrap();
        let plan_ref = cas
            .publish_bytes(&plan.encode_canonical_record(&limits).unwrap())
            .unwrap();

        Self {
            cas,
            cas_root,
            input_ref_root,
            admission_root,
            manifest_ref,
            plan_ref,
            plan,
        }
    }
}

struct FixtureExecution<'a> {
    setup: &'a FixtureSetup,
    population: &'a SourcePopulation,
    published: &'a PublishedPlan,
}
struct FixtureResults {
    nod_root: StreamingOrderedListRoot,
    bucket_root: StreamingOrderedListRoot,
    output_manifest_root: StreamingOrderedListRoot,
    result_chunk_refs: Vec<CasObjectRefV1>,
}
impl FixtureResults {
    fn new(tribute_count: u32, primary_count: u32) -> Self {
        Self {
            nod_root: StreamingOrderedListRoot::new(ListKind::NodActions, tribute_count).unwrap(),
            bucket_root: StreamingOrderedListRoot::new(ListKind::BucketRecords, tribute_count)
                .unwrap(),
            output_manifest_root: StreamingOrderedListRoot::new(
                ListKind::CompleteOutputManifest,
                primary_count,
            )
            .unwrap(),
            result_chunk_refs: Vec::new(),
        }
    }
    fn admit_units(&mut self, execution: &FixtureExecution<'_>) {
        let setup = execution.setup;
        let limits = setup.limits;
        let list_limits = poc_input_list_limits();
        let pinned_bundle = &setup.pinned_bundle;
        let PublishedPlan {
            cas,
            cas_root,
            input_ref_root,
            admission_root,
            manifest_ref,
            plan_ref,
            plan,
        } = execution.published;
        let reader = FilesystemCasReader::open(cas_root, CAS_LIMITS).unwrap();
        let input_refs =
            VerifiedInputChunkRefCatalog::reopen(input_ref_root, &reader, limits, list_limits)
                .unwrap();
        let mut admissions =
            VerifiedAdmissionCatalog::open(admission_root, cas, plan_ref, manifest_ref, limits)
                .unwrap();
        let topology = LysisPlanTopologyV1::new(plan.primary_work_unit_count).unwrap();
        for plan_ordinal in 0..topology.total_unit_count() {
            let spec = {
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
            let (artifact, result_entry) = match topology.plan_position_at(plan_ordinal).unwrap() {
                PlannedUnitPositionV1::TreeNode {
                    phase: UnitPhase::RootReduce,
                    level: 0,
                    index,
                } => self.root_leaf(execution, &spec, index),
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
    }
    fn root_leaf(
        &mut self,
        execution: &FixtureExecution<'_>,
        spec: &UnitSpecV1,
        index: u32,
    ) -> (UnitArtifactV1, Option<OutputManifestEntryV1>) {
        let setup = execution.setup;
        let limits = setup.limits;
        let bundle_hash = setup.bundle_hash;
        let job_id = setup.job_id;
        let tributes = &execution.population.tributes;
        let nod_action_tributes = tributes;
        let contributors_by_owner = &execution.population.contributors_by_owner;
        let cas = &execution.published.cas;
        let start = usize::try_from(index * 256).unwrap();
        let end = (start + 256).min(tributes.len());
        let actions = fixture_support::nod_actions(
            &nod_action_tributes[start..end],
            u32::try_from(start).unwrap(),
        );
        for action in &actions {
            self.nod_root
                .push(
                    &action.encode_canonical_record(&limits).unwrap(),
                    limits.max_bounded_bytes,
                )
                .unwrap();
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
        self.result_chunk_refs.push(chunk_ref.clone());
        let entry = OutputManifestEntryV1 {
            chunk_ordinal: index,
            result_chunk_hash: chunk_hash,
            result_chunk_ref: chunk_ref,
        };
        self.output_manifest_root
            .push(
                &entry.encode_canonical_record(&limits).unwrap(),
                limits.max_bounded_bytes,
            )
            .unwrap();
        let bucket_records = (start..end)
            .map(|ordinal| ordinal.to_be_bytes().to_vec())
            .collect::<Vec<_>>();
        for record in &bucket_records {
            self.bucket_root
                .push(record, limits.max_bounded_bytes)
                .unwrap();
        }
        let summary = fixture_support::root_summary_fixture(
            execution.published.plan.plan_hash(&limits).unwrap(),
            &chunk,
            &tributes[start..end],
            &entry,
            &limits,
        );
        let coverage_root = summary.result_chunk_hashes.tree_root;
        let output_coverage_root = coverage_root;
        (
            UnitArtifactV1::from_canonical_output(
                spec,
                WorkOutputHeaderV1 {
                    source_coverage_root: coverage_root,
                    output_coverage_root,
                    source_coverage_count: 1,
                    output_coverage_count: 1,
                },
                BoundedBytes(
                    encode_root_reduce_output(
                        &RootReduceOutputV1::Leaf {
                            summary,
                            output_manifest_entry: entry.clone(),
                        },
                        &limits,
                    )
                    .unwrap(),
                ),
                &limits,
            )
            .unwrap(),
            Some(entry),
        )
    }
}

pub(super) fn fixture(root: &Path, job_seed: u8, day: WorldwideDay, tribute_count: u32) -> Fixture {
    let _tribute_enclave = outbe_tribute::enclave_client::test_enclave::scope();
    let setup = FixtureSetup::new(job_seed, day, tribute_count);
    let population = SourcePopulation::new(root, &setup);
    let published = PublishedPlan::new(root, &setup, &population);
    let mut results = FixtureResults::new(tribute_count, published.plan.primary_work_unit_count);
    results.admit_units(&FixtureExecution {
        setup: &setup,
        population: &population,
        published: &published,
    });
    drop(published);
    Fixture {
        job_id: setup.job_id,
        day,
        bundle: setup.pinned_bundle,
        nod_root: results.nod_root.finish().unwrap(),
        bucket_root: results.bucket_root.finish().unwrap(),
        output_manifest_root: results.output_manifest_root.finish().unwrap(),
        nod_count: tribute_count,
        result_chunk_refs: results.result_chunk_refs,
        protected_sources: population.protected_sources,
    }
}
