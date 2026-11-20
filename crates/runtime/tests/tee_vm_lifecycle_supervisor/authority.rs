use super::*;
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_tee::TeeAuthorityState;

pub(super) struct Tracker {
    pub(super) channel: outbe_tee::TeeAuthorityChannel,
    probe: Option<outbe_runtime::LifecycleAuthorityProbe>,
    validator: bool,
    storage: HashMapStorageProvider,
    was_jailed: bool,
}
impl Tracker {
    pub(super) fn new(validator: bool) -> Self {
        Self {
            channel: Default::default(),
            probe: None,
            validator,
            storage: HashMapStorageProvider::new(676),
            was_jailed: false,
        }
    }
    fn update_validator(&mut self, jailed: bool, height: u64) {
        self.storage.set_block_number(height);
        StorageHandle::enter(&mut self.storage, |storage| {
            let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage);
            if jailed && !self.was_jailed {
                let owner = Address::repeat_byte(0xA0);
                vs.config_owner.write(owner).unwrap();
                vs.config_is_initialized.write(true).unwrap();
                vs.set_config_max_validators(128).unwrap();
                let minimum = U256::from(1_000);
                vs.test_seed_authoritative_min_stake(minimum).unwrap();
                let public = finality::FinalityCommitteeFixture::new(10).public_keys_min_pk()[0];
                vs.register_validator(owner, Signer::address(), &public)
                    .unwrap();
                vs.test_seed_authoritative_bonded(Signer::address(), minimum)
                    .unwrap();
                vs.activate_validator_via_boundary_for_test(Signer::address())
                    .unwrap();
                vs.jail_validator_for_tee_expiry(Signer::address()).unwrap();
                // Model the finalized boundary removing the jailed validator's old share.
                vs.activate_reshared_set(&[], B256::ZERO).unwrap();
            }
            if !jailed && self.was_jailed {
                let old = vs.get_validator(Signer::address()).unwrap().unwrap();
                vs.unjail_after_stake_check(Signer::address()).unwrap();
                let record = vs.get_validator(Signer::address()).unwrap().unwrap();
                assert_eq!(record.validator_address, old.validator_address);
                assert_eq!(record.consensus_pubkey, old.consensus_pubkey);
                assert_eq!(record.stake, old.stake);
                assert!(!record.has_bls_share);
                assert!(!vs.is_consensus_participant(Signer::address()).unwrap());
                assert!(!vs
                    .get_reshare_target_set()
                    .unwrap()
                    .iter()
                    .any(|r| r.validator_address == Signer::address()));
                // Use the actual production share/Radicle subgate, not a supplied Signer boolean.
                assert!(
                    !outbe_consensus::stack::epoch::signer::radicle_signer_enabled(
                        outbe_radicle::integration::RadicleVotingGate::SignerAllowed,
                        record.has_bls_share
                    )
                    .unwrap()
                );
            }
        });
        self.was_jailed = jailed;
    }
    pub(super) fn observe(&mut self, h: &Harness) -> TeeAuthorityState {
        if !h.live.offer_key_ready().unwrap() || h.chain.binding.is_none() {
            return self.channel.snapshot();
        }
        self.update_validator(h.chain.jailed, h.chain.at.height);
        let binding = h.chain.binding.as_ref().unwrap();
        let (provider, chain, mut identity, hashes) =
            authority_provider::build_tee_admission_mock_provider(
                &[(h.chain.at.height, h.chain.at.timestamp)],
                authority_provider::SeededTeeState {
                    valid_until: binding.valid_until,
                    has_validator_binding: true,
                    validator_binding_mismatch: false,
                    validator_jailed: false,
                    offer_key: h.chain.public,
                },
            );
        identity.validator = self.validator.then(Signer::address);
        identity.reth_p2p_public = h.live.manifest().node_id.reth_p2p_public;
        identity.expected_enclave_id = Some(h.live.manifest().enclave_id().unwrap());
        let node_hash = h.live.manifest().node_id.node_id_hash().unwrap();
        StorageHandle::enter(&mut self.storage, |storage| {
            let registry = outbe_teeregistry::TeeRegistry::new(storage);
            registry
                .v1_node_enclave_id
                .write(&node_hash, identity.expected_enclave_id.unwrap())
                .unwrap();
            registry
                .v1_node_binding_id
                .write(&node_hash, binding.binding_id)
                .unwrap();
            registry
                .v1_node_intent_hash
                .write(&node_hash, binding.intent_hash)
                .unwrap();
            registry
                .v1_node_valid_until
                .write(&node_hash, binding.valid_until)
                .unwrap();
            if let Some(validator) = identity.validator {
                registry
                    .validator_v1_node_hash
                    .write(&validator, node_hash)
                    .unwrap();
            }
        });
        let mut accounts = provider.inner.accounts.lock();
        for ((address, slot), value) in &self.storage.storage {
            let account = accounts
                .get(address)
                .unwrap()
                .clone()
                .extend_storage([(B256::from(slot.to_be_bytes()), *value)]);
            accounts.insert(*address, account);
        }
        drop(accounts);
        if self.probe.is_none() {
            self.probe = Some(
                outbe_runtime::LifecycleAuthorityProbe::new(
                    h.live.clone(),
                    identity,
                    *hashes.last().unwrap(),
                    self.channel.clone(),
                )
                .unwrap(),
            );
        }
        let state = self
            .probe
            .as_ref()
            .unwrap()
            .refresh(&provider, &outbe_runtime::OutbeRuntime::new(), &chain)
            .unwrap();
        assert_eq!(self.channel.snapshot(), state);
        state
    }
}
