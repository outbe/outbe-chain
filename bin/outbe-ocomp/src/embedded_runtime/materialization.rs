use super::*;
use outbe_ocomp_protocol::nod_materialization::{
    NodMaterializationHeadV1, ProtectedNodMaterializationV2,
};

pub(super) struct MaterializationJob {
    pub(super) submission_gate: Arc<ValidatorOcompSubmissionGateV1>,
    pub(super) rpc_url: String,
    pub(super) signer: OutbeEvmSigner,
    pub(super) sender_address: alloy_primitives::Address,
    pub(super) chain_id: u64,
    pub(super) limits: SchemaLimits,
    pub(super) cas_root: PathBuf,
    pub(super) cas_limits: CasLimits,
    pub(super) input_ref_root: PathBuf,
    pub(super) job_root: PathBuf,
    pub(super) bundle: PinnedProtocolBundle,
    pub(super) reference_root: PathBuf,
    pub(super) submission_root: PathBuf,
    pub(super) head: NodMaterializationHeadV1,
    pub(super) batch_subtree_height: u8,
}

struct PreparedMaterialization {
    references: MaterializationReferenceStoreV1,
    protected: ProtectedNodMaterializationV2,
    // Keep both catalog locks until submission ends.
    _admissions: VerifiedAdmissionCatalog,
    _input_refs: VerifiedInputChunkRefCatalog,
}

impl MaterializationJob {
    pub(super) fn run(self) -> Result<bool, EmbeddedOcompRuntimeErrorV1> {
        let _submission_permit = self.submission_gate.acquire()?;
        let prepared = self.prepare()?;
        let job_id = self.head.job_id;
        let rpc = PublicVoteRpcClientV1::new(self.rpc_url, RPC_MAX_RESPONSE_BYTES)
            .map_err(|error| stage("open NOD materialization RPC", error))?;
        let mut submitter = NodMaterializationSubmitterV1::open(
            NodMaterializationSubmissionConfigV1 {
                journal_root: self.submission_root,
                expected_chain_id: self.chain_id,
                sender_address: self.sender_address,
                limits: self.limits,
            },
            rpc,
            self.signer,
        )
        .map_err(|error| stage("open NOD materialization submitter", error))?;
        loop {
            match submitter.reconcile(job_id, &prepared.protected) {
                Ok(NodMaterializationSubmissionOutcomeV1::Pending) => {
                    thread::sleep(RETRY_INTERVAL);
                }
                Ok(NodMaterializationSubmissionOutcomeV1::Finalized { success }) => {
                    prepared
                        .references
                        .release(job_id)
                        .map_err(|error| stage("release NOD materialization references", error))?;
                    return Ok(success);
                }
                Err(_error) => {
                    thread::sleep(RETRY_INTERVAL);
                }
            }
        }
    }

    fn prepare(&self) -> Result<PreparedMaterialization, EmbeddedOcompRuntimeErrorV1> {
        let job_id = self.head.job_id;
        let head = &self.head;
        let batch_subtree_height = self.batch_subtree_height;
        let chain_id = self.chain_id;
        let limits = self.limits;
        let cas_root = &self.cas_root;
        let cas_limits = self.cas_limits;
        let input_ref_root = &self.input_ref_root;
        let job_root = &self.job_root;
        let bundle = &self.bundle;
        let reference_root = &self.reference_root;
        let submission_root = &self.submission_root;
        let reader = FilesystemCasReader::open(cas_root, cas_limits)
            .map_err(|error| stage("open NOD materialization CAS", error))?;
        let job_component = hex::encode(job_id.as_slice());
        let input_refs = VerifiedInputChunkRefCatalog::reopen(
            input_ref_root.join(&job_component),
            &reader,
            limits,
            poc_input_list_limits(),
        )
        .map_err(|error| stage("open NOD materialization inputs", error))?;
        let admissions = VerifiedAdmissionCatalog::reopen(
            job_root.join(&job_component).join("admissions"),
            &reader,
            limits,
        )
        .map_err(|error| stage("open NOD materialization admissions", error))?;
        let audit = LocalLysisPlanAuditV1::open(&admissions, &input_refs, &reader, bundle, &limits)
            .map_err(|error| stage("audit NOD materialization plan", error))?;
        let mut built =
            build_nod_materialization_batch_with_references(&audit, head, batch_subtree_height)
                .map_err(|error| stage("build NOD materialization batch", error))?;
        let inventory_root = input_ref_root
            .join(".work")
            .join(&job_component)
            .join("inventory");
        let (sources, source_references) =
            materialization_sources(&audit, &input_refs, &built, &inventory_root)
                .map_err(|error| stage("authenticate NOD encryption sources", error))?;
        built.dependencies.extend(source_references);
        crate::nod_materialization::normalize_dependencies(&mut built.dependencies)
            .map_err(|error| stage("normalize NOD materialization references", error))?;
        let references = MaterializationReferenceStoreV1::open(reference_root)
            .map_err(|error| stage("open NOD materialization references", error))?;
        references
            .pin_exact(job_id, &built.dependencies)
            .map_err(|error| stage("pin NOD materialization references", error))?;
        let preparation = PreparedNodMaterializationStoreV2::open(submission_root, limits)
            .map_err(|error| stage("open encrypted NOD preparation", error))?;
        let protected = prepare_protected_materialization(
            outbe_tee::nod_materialization::NodMaterializationAuthorityV2 {
                chain_id,
                head: head
                    .encode_canonical(&limits)
                    .map_err(|error| stage("encode NOD generation authority", error))?,
                subtree_height: batch_subtree_height,
                sealed_tribute_root: audit.manifest().sealed_tribute_collection_root,
            },
            head,
            &built,
            sources,
            &preparation,
        )
        .map_err(|error| stage("prepare encrypted NOD materialization", error))?;
        Ok(PreparedMaterialization {
            references,
            protected,
            _admissions: admissions,
            _input_refs: input_refs,
        })
    }
}
