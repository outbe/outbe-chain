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
}

/// Fixture finality; production snapshot reader still resolves the exact canonical hash.
#[derive(Clone, Default)]
pub struct FinalizedProvider(Arc<RwLock<FinalizedStates>>);

impl FinalizedProvider {
    pub fn finalize(&self, number: u64, storage: &HashMapStorageProvider) {
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
        let mut bytes = [0; 32];
        bytes[24..].copy_from_slice(&number.to_be_bytes());
        let hash = B256::from(bytes);
        let mut states = self.0.write().unwrap();
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
