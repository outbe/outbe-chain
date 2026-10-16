use alloy_eips::{BlockNumHash, BlockNumberOrTag};
use alloy_primitives::{Address, B256, U256};
use commonware_actor::{Feedback, Unreliable};
use commonware_cryptography::bls12381::PublicKey;
use commonware_p2p::{CheckedSender, LimitedSender, Receiver, Recipients};
use commonware_runtime::{IoBuf, IoBufs};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use reth_chainspec::ChainInfo;
use reth_provider::{
    test_utils::{ExtendedAccount, MockEthProvider},
    BlockHashReader, BlockIdReader, BlockNumReader, ProviderError, ProviderResult,
    StateProviderBox, StateProviderFactory,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
    time::SystemTime,
};
use tokio::sync::mpsc;

#[derive(Default)]
struct FinalizedStates {
    states: BTreeMap<B256, MockEthProvider>,
    canonical: BTreeMap<u64, B256>,
    log_cursor: usize,
}

/// Fixture finality; production snapshot reader still resolves the exact canonical hash.
#[derive(Clone, Default)]
pub struct FinalizedProvider(Arc<RwLock<FinalizedStates>>);

impl FinalizedProvider {
    pub fn finalize(&self, number: u64, storage: &mut HashMapStorageProvider) {
        let mut bytes = [0; 32];
        bytes[24..].copy_from_slice(&number.to_be_bytes());
        let hash = B256::from(bytes);
        storage.set_canonical_block_hash(number, hash);
        let state = MockEthProvider::new();
        let mut accounts: BTreeMap<Address, Vec<(B256, U256)>> = BTreeMap::new();
        for ((address, slot), value) in &storage.storage {
            accounts
                .entry(*address)
                .or_default()
                .push((B256::from(slot.to_be_bytes::<32>()), *value));
        }
        for (address, slots) in accounts {
            state.add_account(
                address,
                ExtendedAccount::new(0, U256::ZERO).extend_storage(slots),
            );
        }
        let mut states = self.0.write().unwrap();
        let logs = storage.get_ordered_events();
        state.add_receipts(
            number,
            vec![alloy_consensus::EthereumReceipt {
                tx_type: alloy_consensus::TxType::Legacy,
                success: true,
                cumulative_gas_used: 1,
                logs: logs[states.log_cursor..].to_vec(),
            }],
        );
        states.log_cursor = logs.len();
        assert!(states.states.insert(hash, state).is_none());
        assert!(states.canonical.insert(number, hash).is_none());
    }
}

impl BlockHashReader for FinalizedProvider {
    fn block_hash(&self, number: u64) -> ProviderResult<Option<B256>> {
        Ok(self.0.read().unwrap().canonical.get(&number).copied())
    }
    fn canonical_hashes_range(&self, start: u64, end: u64) -> ProviderResult<Vec<B256>> {
        Ok(self
            .0
            .read()
            .unwrap()
            .canonical
            .range(start..end)
            .map(|(_, hash)| *hash)
            .collect())
    }
}
impl reth_provider::ReceiptProvider for FinalizedProvider {
    type Receipt = <MockEthProvider as reth_provider::ReceiptProvider>::Receipt;
    fn receipt(&self, _: u64) -> ProviderResult<Option<Self::Receipt>> {
        panic!("wrong receipt API")
    }
    fn receipt_by_hash(&self, _: B256) -> ProviderResult<Option<Self::Receipt>> {
        panic!("wrong receipt API")
    }
    fn receipts_by_block(
        &self,
        block: alloy_eips::BlockHashOrNumber,
    ) -> ProviderResult<Option<Vec<Self::Receipt>>> {
        let alloy_eips::BlockHashOrNumber::Hash(hash) = block else {
            panic!("receipts must use exact hash")
        };
        let states = self.0.read().unwrap();
        let number = states
            .canonical
            .iter()
            .find_map(|(n, h)| (*h == hash).then_some(*n))
            .ok_or(ProviderError::BestBlockNotFound)?;
        states
            .states
            .get(&hash)
            .ok_or(ProviderError::BestBlockNotFound)?
            .receipts_by_block(number.into())
    }
    fn receipts_by_tx_range(
        &self,
        _: impl std::ops::RangeBounds<u64>,
    ) -> ProviderResult<Vec<Self::Receipt>> {
        panic!("wrong receipt API")
    }
    fn receipts_by_block_range(
        &self,
        _: std::ops::RangeInclusive<u64>,
    ) -> ProviderResult<Vec<Vec<Self::Receipt>>> {
        panic!("wrong receipt API")
    }
}
impl BlockNumReader for FinalizedProvider {
    fn chain_info(&self) -> ProviderResult<ChainInfo> {
        let states = self.0.read().unwrap();
        let (&best_number, &best_hash) = states.canonical.last_key_value().unwrap();
        Ok(ChainInfo {
            best_hash,
            best_number,
        })
    }
    fn best_block_number(&self) -> ProviderResult<u64> {
        Ok(self.chain_info()?.best_number)
    }
    fn last_block_number(&self) -> ProviderResult<u64> {
        self.best_block_number()
    }
    fn block_number(&self, hash: B256) -> ProviderResult<Option<u64>> {
        Ok(self
            .0
            .read()
            .unwrap()
            .canonical
            .iter()
            .find_map(|(n, h)| (*h == hash).then_some(*n)))
    }
}
impl BlockIdReader for FinalizedProvider {
    fn pending_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(None)
    }
    fn safe_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(None)
    }
    fn finalized_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        let info = self.chain_info()?;
        Ok(Some(BlockNumHash::new(info.best_number, info.best_hash)))
    }
}
impl StateProviderFactory for FinalizedProvider {
    fn state_by_block_hash(&self, hash: B256) -> ProviderResult<StateProviderBox> {
        self.0
            .read()
            .unwrap()
            .states
            .get(&hash)
            .ok_or(ProviderError::BestBlockNotFound)?
            .latest()
    }
    fn latest(&self) -> ProviderResult<StateProviderBox> {
        panic!("must resolve exact finalized hash")
    }
    fn state_by_block_number_or_tag(
        &self,
        _: BlockNumberOrTag,
    ) -> ProviderResult<StateProviderBox> {
        panic!("must resolve exact finalized hash")
    }
    fn history_by_block_number(&self, _: u64) -> ProviderResult<StateProviderBox> {
        panic!("must resolve exact finalized hash")
    }
    fn history_by_block_hash(&self, _: B256) -> ProviderResult<StateProviderBox> {
        panic!("wrong state API")
    }
    fn pending(&self) -> ProviderResult<StateProviderBox> {
        panic!("must not read pending state")
    }
    fn pending_state_by_hash(&self, _: B256) -> ProviderResult<Option<StateProviderBox>> {
        panic!("must not read pending state")
    }
    fn maybe_pending(&self) -> ProviderResult<Option<StateProviderBox>> {
        panic!("must not read pending state")
    }
}

pub type Inbox = mpsc::UnboundedSender<(PublicKey, IoBuf)>;
pub type Routes = Arc<BTreeMap<PublicKey, Inbox>>;

/// In-process authenticated Commonware transport fixture; native sidecars use real Noise/TCP.
#[derive(Clone)]
pub struct RoutedSender {
    pub sender: PublicKey,
    pub routes: Routes,
    pub recipient: Option<PublicKey>,
}
impl LimitedSender for RoutedSender {
    type PublicKey = PublicKey;
    type Checked<'a> = Self;
    fn check(&mut self, recipients: Recipients<PublicKey>) -> Result<Self, SystemTime> {
        let Recipients::One(peer) = recipients else {
            return Err(SystemTime::now());
        };
        if !self.routes.contains_key(&peer) {
            return Err(SystemTime::now());
        }
        Ok(Self {
            sender: self.sender.clone(),
            routes: self.routes.clone(),
            recipient: Some(peer),
        })
    }
}
impl CheckedSender for RoutedSender {
    type PublicKey = PublicKey;
    fn recipients(&self) -> Vec<PublicKey> {
        self.recipient.iter().cloned().collect()
    }
    fn send(self, message: impl Into<IoBufs> + Send, _: bool) -> Unreliable<Feedback> {
        use bytes::Buf as _;
        let mut message = message.into();
        let bytes = message.copy_to_bytes(message.remaining());
        self.routes[&self.recipient.unwrap()]
            .send((self.sender, bytes.into()))
            .unwrap();
        Unreliable::Outcome(Feedback::Ok)
    }
}
#[derive(Debug)]
pub struct RoutedReceiver(pub mpsc::UnboundedReceiver<(PublicKey, IoBuf)>);
impl Receiver for RoutedReceiver {
    type PublicKey = PublicKey;
    type Error = std::io::Error;
    async fn recv(&mut self) -> Result<(PublicKey, IoBuf), Self::Error> {
        self.0.recv().await.ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "fixture transport stopped")
        })
    }
}

/// Real cryptographic resharing with fixture delivery; no chain DKG actor/finality is simulated here.
pub fn reshare(
    keys: &[commonware_cryptography::bls12381::PrivateKey],
    previous: &outbe_consensus::bls::ParticipantDkgBootstrapResult,
) -> outbe_consensus::bls::ParticipantDkgBootstrapResult {
    use commonware_codec::Encode as _;
    use commonware_cryptography::bls12381::{
        dkg::feldman_desmedt::{Dealer, Info, Logs, Player, Reveal},
        primitives::{sharing::Mode, variant::MinSig},
    };
    use commonware_cryptography::{bls12381, Signer as _};
    use commonware_parallel::Sequential;
    use commonware_utils::{N3f1, TryCollect as _};
    let participants = keys
        .iter()
        .map(|key| key.public_key())
        .try_collect()
        .unwrap();
    let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
        &outbe_consensus::config::outbe_app_namespace(),
        1,
        Some(previous.output.clone()),
        Mode::NonZeroCounter,
        Reveal::V1,
        participants,
        previous.output.players().clone(),
    )
    .unwrap();
    let mut dealers = Vec::new();
    let mut public_messages = Vec::new();
    let mut private_messages = Vec::new();
    for (index, key) in keys.iter().enumerate() {
        let (dealer, public, private) = Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
            rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng),
            info.clone(),
            key.clone(),
            Some(previous.shares[index].clone()),
        )
        .unwrap();
        dealers.push(dealer);
        public_messages.push(public);
        private_messages.push(private);
    }
    let mut players: Vec<_> = keys
        .iter()
        .map(|key| Player::new(info.clone(), key.clone()).unwrap())
        .collect();
    outbe_consensus::test_harness::acknowledge_fixture_dealings(
        keys,
        outbe_consensus::test_harness::FixtureDealings {
            public_messages: &public_messages,
            private_messages: &private_messages,
        },
        &mut dealers,
        &mut players,
    );
    let mut logs = BTreeMap::new();
    for dealer in dealers {
        let (key, log) = dealer.finalize::<N3f1>().check(&info).unwrap();
        logs.insert(key, log);
    }
    let mut results = players.into_iter().map(|player| {
        let mut selected = Logs::<MinSig, bls12381::PublicKey, N3f1>::new(info.clone());
        for (key, log) in logs.clone() {
            selected.record(key, log);
        }
        player
            .finalize::<N3f1, bls12381::Batch>(
                &mut rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng),
                selected,
                &Sequential,
            )
            .unwrap()
    });
    let (output, first) = results.next().unwrap();
    let mut shares = vec![first];
    for (other, share) in results {
        assert_eq!(other.encode(), output.encode());
        shares.push(share);
    }
    assert_eq!(output.public().public(), previous.output.public().public());
    assert_ne!(output.public(), previous.output.public());
    outbe_consensus::bls::ParticipantDkgBootstrapResult {
        polynomial: output.public().clone(),
        output,
        shares,
    }
}
