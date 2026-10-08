use super::*;

#[derive(Default)]
pub(super) struct LaneIdentities {
    worker_addresses: std::collections::BTreeSet<std::net::SocketAddr>,
    expected_identity: Option<(u64, B256)>,
    pub(super) expected_key_hash: Option<B256>,
}

impl LaneIdentities {
    pub(super) fn check(
        &mut self,
        bundle_config: &EmbeddedOcompBundleConfigV1,
    ) -> Result<(), EmbeddedOcompRuntimeErrorV1> {
        if !bundle_config.worker_address.ip().is_loopback()
            || (bundle_config.worker_address.port() != 0
                && !self.worker_addresses.insert(bundle_config.worker_address))
            || bundle_config.identity.protocol_bundle_hash != bundle_config.protocol_bundle.hash()
        {
            return Err(EmbeddedOcompRuntimeErrorV1::InvalidConfig);
        }
        let identity_pair = (
            bundle_config.identity.chain_id,
            bundle_config.identity.genesis_hash,
        );
        if self
            .expected_identity
            .replace(identity_pair)
            .is_some_and(|expected| expected != identity_pair)
        {
            return Err(EmbeddedOcompRuntimeErrorV1::InvalidConfig);
        }
        Ok(())
    }
}

pub(super) struct LaneAssembly<'a> {
    pub(super) config: &'a EmbeddedOcompDomainConfigV1,
    pub(super) layout: &'a EmbeddedOcompLayoutV1,
    pub(super) cas_limits: CasLimits,
    pub(super) submission_gate: Arc<ValidatorOcompSubmissionGateV1>,
}

impl LaneAssembly<'_> {
    pub(super) fn open(
        &self,
        bundle_config: &EmbeddedOcompBundleConfigV1,
        expected_key_hash: &mut Option<B256>,
    ) -> Result<EmbeddedOcompBundleLaneV1, EmbeddedOcompRuntimeErrorV1> {
        let config = self.config;
        let layout = self.layout;
        let cas_limits = self.cas_limits;
        let bundle_hash = bundle_config.protocol_bundle.hash();
        let worker_server = SupervisorWorkerServerV1::start(
            bundle_config.worker_address,
            bundle_config.identity,
            config.registry_generation,
            config.limits,
        )
        .map_err(|error| stage("start Node-owned OCOMP Worker endpoint", error))?;
        let runner = Arc::new(
            SupervisorJobRunnerV1::open(
                SupervisorJobRunnerConfigV1 {
                    cas_root: layout.cas_root.clone(),
                    cas_limits,
                    input_ref_root: layout.input_ref_root.clone(),
                    job_root: layout.job_root.clone(),
                    worker_inbox_root: layout
                        .worker_inbox_root
                        .join(hex::encode(bundle_hash.as_slice())),
                    worker_inbox_limits: WorkerInboxLimits {
                        max_artifact_bytes: WORKER_INBOX_MAX_ARTIFACT_BYTES,
                        max_total_bytes: WORKER_INBOX_MAX_TOTAL_BYTES,
                    },
                    protocol_bundle: bundle_config.protocol_bundle.clone(),
                    limits: config.limits,
                },
                worker_server.dispatcher(),
            )
            .map_err(|error| stage("open embedded OCOMP runner", error))?,
        );
        let adoption = SupervisorExportAdoptionConfig {
            cas_root: layout.cas_root.clone(),
            cas_limits,
            input_ref_root: layout.input_ref_root.clone(),
            receipt_root: layout.receipt_root.clone(),
            binding_root: layout.binding_root.clone(),
            protocol_bundle: bundle_config.protocol_bundle.clone(),
            limits: config.limits,
        };
        let validator_ocomp = self.validator_policy(bundle_config, expected_key_hash)?;
        Ok(EmbeddedOcompBundleLaneV1 {
            _worker_server: worker_server,
            runner,
            adoption,
            validator_ocomp,
        })
    }

    fn validator_policy(
        &self,
        bundle_config: &EmbeddedOcompBundleConfigV1,
        expected_key_hash: &mut Option<B256>,
    ) -> Result<Option<ValidatorOcompPolicyV1>, EmbeddedOcompRuntimeErrorV1> {
        if self.config.policy == EmbeddedNodePolicyV1::FullNode {
            return Ok(None);
        }
        let config = self.config;
        let layout = self.layout;
        let cas_limits = self.cas_limits;
        let submission_gate = &self.submission_gate;
        reconcile_finalized_materialization_references(
            &layout.materialization_submission_root,
            &layout.materialization_reference_root,
        )
        .map_err(|error| stage("reconcile finalized NOD materialization references", error))?;
        let rpc_url = config
            .validator_rpc_url
            .clone()
            .ok_or(EmbeddedOcompRuntimeErrorV1::MissingValidatorRpc)?;
        let owner_uid =
            effective_uid().map_err(|error| stage("resolve Node effective uid", error))?;
        let evm_signer =
            outbe_primitives::signer::load::from_strict_file(&layout.evm_key_path, owner_uid)
                .map_err(|error| stage("open Validator OCOMP EVM key", error))?;
        let result_signer = OcompSigner::from_file(&layout.result_key_path, owner_uid)
            .map_err(|error| stage("open Validator OCOMP result key", error))?;
        let sign_once =
            SignOnceStore::open(layout.sign_once_root.clone(), owner_uid, config.limits)
                .map_err(|error| stage("open Validator OCOMP sign-once store", error))?;
        let attester = LocalResultVoteAttesterV1::new(
            bundle_config.identity,
            bundle_config.protocol_bundle.bundle().fork_id,
            result_signer,
            sign_once,
            config.limits,
        )
        .map_err(|error| stage("open Validator OCOMP result attester", error))?;
        let ocomp_key_hash = attester.ocomp_key_hash();
        if expected_key_hash
            .replace(ocomp_key_hash)
            .is_some_and(|expected| expected != ocomp_key_hash)
        {
            return Err(EmbeddedOcompRuntimeErrorV1::InvalidConfig);
        }
        let preparer = Arc::new(
            LocalVoteTransactionPreparerV1::new(
                evm_signer.clone(),
                attester,
                bundle_config.identity.chain_id,
                config.limits,
            )
            .map_err(|error| stage("open Validator OCOMP vote preparer", error))?,
        );
        Ok(Some(ValidatorOcompPolicyV1 {
            sender_address: preparer.sender_address(),
            preparer,
            materialization_signer: evm_signer,
            submission_gate: Arc::clone(submission_gate),
            ocomp_key_hash,
            rpc_url,
            journal_root: layout.vote_submission_root.clone(),
            payout_journal_root: layout.payout_submission_root.clone(),
            materialization_reference_root: layout.materialization_reference_root.clone(),
            materialization_submission_root: layout.materialization_submission_root.clone(),
            cas_root: layout.cas_root.clone(),
            cas_limits,
            input_ref_root: layout.input_ref_root.clone(),
            job_root: layout.job_root.clone(),
            protocol_bundle: bundle_config.protocol_bundle.clone(),
            chain_id: bundle_config.identity.chain_id,
            limits: config.limits,
        }))
    }
}
