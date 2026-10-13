use super::*;

pub(super) struct PendingScenario {
    pub(super) public: PathBuf,
    pub(super) chain_root: PathBuf,
    signer: OutbeEvmSigner,
    own_evm_key: Vec<u8>,
    own_result_key: Vec<u8>,
    pub(super) fixture: Fixture,
    pub(super) closed: ProjectionCheckpoint,
    day: u32,
}
pub(super) struct ScenarioRpc {
    pub(super) rpc: ScriptedRpc,
    pub(super) active: Vec<u8>,
    pub(super) replies: BTreeMap<(Address, Vec<u8>), Vec<u8>>,
}

pub(super) fn prepare(receiver: &Path) -> PendingScenario {
    let public = receiver.join("ocomp");
    let chain_root = receiver.join("chain");
    let (signer, _, _) = resident_keys(&public);
    let own_evm_key = fs::read(public.join("ocomp-evm-key.hex")).unwrap();
    let own_result_key = fs::read(public.join("ocomp-key-v1.hex")).unwrap();
    let donor = tempfile::tempdir().unwrap();
    let donor_chain = donor.path().join("chain");
    let donor_public = donor.path().join("ocomp");
    let h = signed_frames(&donor_chain, 0, H, &signer)[H as usize];
    let day = outbe_primitives::time::worldwide_day_from_timestamp(H);
    let f = fixture(&donor_public, 0x71, WorldwideDay::from(day), 257);
    seed_native(&donor_chain, &f, signer.address());
    let mut donor_runtime = copied_native::runtime(
        copied_native::provider(&donor_chain),
        &donor_public,
        f.bundle.clone(),
    );
    copied_native::catch_up(&mut donor_runtime, h);
    assert!(donor_runtime.jobs.is_empty());
    assert_eq!(donor_runtime.closure_checkpoint.current().unwrap(), h);
    drop(donor_runtime);
    // A distinct donor key makes accidental identity copying detectable.
    write_key(&donor_public.join("ocomp-evm-key.hex"), 0x61);
    write_key(&donor_public.join("ocomp-key-v1.hex"), 0x62);
    let copy = PublicCopy {
        donor,
        fixture: f,
        closed: h,
    };
    copy.place_public_files(receiver);
    let PublicCopy {
        donor,
        fixture: f,
        closed: _,
    } = copy;
    donor.close().unwrap();
    assert_eq!(
        fs::read(public.join("ocomp-evm-key.hex")).unwrap(),
        own_evm_key
    );
    assert_eq!(
        fs::read(public.join("ocomp-key-v1.hex")).unwrap(),
        own_result_key
    );
    PendingScenario {
        public,
        chain_root,
        signer,
        own_evm_key,
        own_result_key,
        fixture: f,
        closed: h,
        day,
    }
}

pub(super) async fn drive_eligible_frames<P>(
    resumed: &mut EmbeddedOcompExExV1<P>,
    k: ProjectionCheckpoint,
) where
    P: reth_provider::BlockIdReader
        + reth_provider::BlockHashReader
        + reth_provider::BlockReader
        + reth_provider::ReceiptProvider<Receipt = OutbeReceipt>
        + StateProviderFactory
        + Clone
        + Send
        + Sync
        + 'static,
{
    let source = RethFinalizedFrameSource::new(resumed.provider.clone());
    let mut visited = Vec::new();
    while let Some(batch) = read_bounded_finalized_frames(
        &source,
        resumed.scanned_height + 1,
        (k.block_number, k.block_hash).into(),
    )
    .unwrap()
    {
        for frame in batch.frames() {
            visited.push(frame.identity().number);
            resumed.record_scanned_frame(frame).unwrap();
            if frame.identity().number == k.block_number {
                resumed
                    .refresh_jobs(k.block_number, k.block_hash, false)
                    .await
                    .unwrap();
                // Actual effect entrypoints, including finalized proposer
                // recovery, native role resolution, and detached workers.
                resumed.reconcile_materialization(frame).unwrap();
            }
        }
        resumed.flush_closure_checkpoint().unwrap();
    }
    // The frame reader owns a provider clone. Close it before the
    // final ordinary reopen of this same native MDBX environment.
    drop(source);
    assert_eq!(visited, (H + 1..=k.block_number).collect::<Vec<_>>());
    assert_eq!(resumed.closure_checkpoint.current().unwrap(), k);
}

impl PendingScenario {
    pub(super) fn start_rpc(&self) -> ScenarioRpc {
        let public = &self.public;
        let chain_root = &self.chain_root;
        let f = &self.fixture;
        let h = self.closed;
        let signer = &self.signer;
        let head = read_native_pending_head(&copied_native::provider(chain_root));
        assert_eq!(head.next_nod_ordinal, 256);
        assert_eq!(head.nod_count, 257);
        assert_eq!(head.last_progress_height, H);
        let subtree = outbe_chain_constants::get_nod_materialization_batch_subtree_height();
        // Existing build_remaining fixture currently uses subtree height 3.
        // This is the ordinary default, not a production timing override.
        assert_eq!(subtree, 3);
        let expected_nod = encode_protected_materialize_certified_nods_calldata(
            &protected_batch(f, &build_remaining(public, f, &head).unwrap().batch),
            &poc_schema_limits(),
        )
        .unwrap();
        let active = scripted_active(f);
        let replies = scripted_active_replies(f, active.clone());
        let rpc = ScriptedRpc::start(
            h,
            signer.address(),
            replies.clone(),
            BTreeMap::from([(NOD_FACTORY_ADDRESS, expected_nod)]),
        );
        ScenarioRpc {
            rpc,
            active,
            replies,
        }
    }
    pub(super) async fn assert_quiet_finality(&self, validator: bool, rpc: &ScriptedRpc) {
        let public = &self.public;
        let chain_root = &self.chain_root;
        let f = &self.fixture;
        let h = self.closed;
        let mut quiet = copied_native::runtime(
            copied_native::provider(chain_root),
            public,
            f.bundle.clone(),
        );
        if validator {
            enable_validator(&mut quiet, public, &f.bundle, &rpc.url);
        }
        assert_eq!(quiet.closure_checkpoint.current().unwrap(), h);
        assert!(quiet.jobs.is_empty() && quiet.requests.is_empty());
        // The quiet C=H branch in run.rs refreshes jobs. It does not call the
        // effect drivers without a new finalized frame. Empty requests model
        // pruned terminal jobs, not successful Completed verification.
        quiet.refresh_jobs(H, h.block_hash, true).await.unwrap();
        quiet.flush_closure_checkpoint().unwrap();
        assert_eq!(quiet.closure_checkpoint.current().unwrap(), h);
        assert!(quiet.materialization_active.is_none());
        assert!(matches!(
            quiet.materialization_rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        rpc.assert_quiet();
        assert_no_submission_files(public);
        drop(quiet);
    }
    pub(super) fn advance_retry_frame(
        &self,
        rpc: &ScriptedRpc,
        active: Vec<u8>,
        replies: BTreeMap<(Address, Vec<u8>), Vec<u8>>,
    ) -> ProjectionCheckpoint {
        let chain_root = &self.chain_root;
        let f = &self.fixture;
        let signer = &self.signer;
        let day = self.day;
        let retry = outbe_chain_constants::get_nod_materialization_retry_interval_blocks();
        assert!(retry > 0);
        let k_height = H.checked_add(retry).unwrap();
        let k = *signed_frames(chain_root, H + 1, k_height, signer)
            .last()
            .unwrap();
        assert_eq!(
            outbe_primitives::time::worldwide_day_from_timestamp(k_height),
            day
        );
        // Native frames are not executed. The scripted active-generation reply stays put.
        assert_eq!(scripted_active_replies(f, active), replies);
        *rpc.point.lock().unwrap() = k;
        k
    }

    pub(super) fn assert_validator_results<P>(
        &self,
        resumed: &mut EmbeddedOcompExExV1<P>,
        rpc: &ScriptedRpc,
    ) where
        P: reth_provider::BlockIdReader
            + reth_provider::BlockHashReader
            + reth_provider::BlockReader
            + reth_provider::ReceiptProvider<Receipt = OutbeReceipt>
            + StateProviderFactory
            + Clone
            + Send
            + Sync
            + 'static,
    {
        let f = &self.fixture;
        assert!(resumed.materialization_active.is_some());
        // Drop the original: after the result, Disconnected proves the
        // producer released its channel. The isolated process bounds a
        // worker stuck before that point. No worker join API is invented.
        drop(std::mem::replace(
            &mut resumed.materialization_tx,
            std::sync::mpsc::channel().0,
        ));
        let nod = resumed
            .materialization_rx
            .recv_timeout(Duration::from_secs(45))
            .expect("actual materialization result");
        match &nod {
            EmbeddedMaterializationOutcomeV1::Finalized {
                job_id,
                queue_sequence,
                first_nod_ordinal,
                success,
            } => {
                assert_eq!(*job_id, f.job_id);
                assert_eq!(*queue_sequence, 1);
                assert_eq!(*first_nod_ordinal, 256);
                assert!(!success);
            }
            EmbeddedMaterializationOutcomeV1::Unavailable { detail, .. } => {
                panic!("actual materialization unavailable: {detail}")
            }
        }
        resumed.handle_materialization(nod);
        assert!(matches!(
            resumed
                .materialization_rx
                .recv_timeout(Duration::from_secs(2)),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
        ));
        assert!(resumed.materialization_active.is_none());
        let evidence = rpc.evidence.lock().unwrap();
        assert_eq!(evidence.sent.len(), 1);
    }
    pub(super) fn assert_fullnode_results<P>(
        &self,
        resumed: &EmbeddedOcompExExV1<P>,
        rpc: &ScriptedRpc,
    ) {
        let public = &self.public;
        assert!(resumed.materialization_active.is_none());
        assert!(matches!(
            resumed.materialization_rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        rpc.assert_quiet();
        assert_no_submission_files(public);
        assert_eq!(resumed.domain.validator_sender_address(), None);
    }
    pub(super) fn assert_reopened(&self, k: ProjectionCheckpoint) {
        let public = &self.public;
        let chain_root = &self.chain_root;
        let f = &self.fixture;
        let own_evm_key = &self.own_evm_key;
        let own_result_key = &self.own_result_key;
        let reopened = copied_native::runtime(
            copied_native::provider(chain_root),
            public,
            f.bundle.clone(),
        );
        assert_eq!(reopened.closure_checkpoint.current().unwrap(), k);
        assert_eq!(
            read_native_pending_head(&reopened.provider).next_nod_ordinal,
            256
        );
        assert_eq!(
            fs::read(public.join("ocomp-evm-key.hex")).unwrap(),
            *own_evm_key
        );
        assert_eq!(
            fs::read(public.join("ocomp-key-v1.hex")).unwrap(),
            *own_result_key
        );
        drop(reopened);
    }
}
