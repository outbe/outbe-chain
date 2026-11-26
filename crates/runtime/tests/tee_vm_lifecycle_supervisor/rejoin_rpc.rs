//! RPC projection of the signed execution ledger; latest and finalized advance independently.
use crate::{
    execution_committee::Committee,
    moving_head_rpc::{block_json, schedule_for},
    rejoin_execution::Ledger,
    Signer,
};
use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use commonware_codec::Encode as _;
use evm2::TxResult;
use jsonrpsee::{types::ErrorObjectOwned, Methods, RpcModule};
use outbe_consensus::block::ConsensusBlock;
use outbe_evm::factory::OutbeEvmTypes;
use outbe_tee::{
    finalized_admission::FinalizedAdmissionWitnessV1,
    lifecycle::{LifecycleCheckpointV1, ServiceTxKindV1, ServiceTxRecordV1, ServiceTxStateV1},
};
use reth_primitives_traits::SealedBlock;
use serde_json::{json, Value};
use std::{collections::HashMap, path::PathBuf, sync::Mutex};

pub(super) struct RejoinChain {
    pub(super) latest: Ledger,
    pub(super) finalized: Ledger,
    pub(super) sent: Vec<Bytes>,
    pub(super) retired_renewal: Option<B256>,
    headers: Vec<Value>,
    blocks: Vec<ConsensusBlock>,
    receipts: HashMap<B256, Value>,
    committee: Committee,
    journal_path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
enum RejoinRpcError {
    #[error("unknown canonical block selector")]
    Block,
    #[error("unexpected account {0}")]
    Account(Address),
    #[error("contract call reverted for {0}")]
    Call(Address),
    #[error("expected registry proof at finalized head")]
    Proof,
}

impl From<RejoinRpcError> for ErrorObjectOwned {
    fn from(error: RejoinRpcError) -> Self {
        Self::owned(-32000, error.to_string(), None::<()>)
    }
}

impl RejoinChain {
    pub(super) fn new(mut ledger: Ledger, committee: Committee, data_dir: PathBuf) -> Self {
        ledger.set_boundary_outcome(&committee);
        let genesis = LifecycleCheckpointV1 {
            height: 0,
            hash: ledger.tip.genesis_hash,
            ..ledger.tip
        };
        let mut chain = Self {
            finalized: ledger.clone(),
            latest: ledger,
            sent: Vec::new(),
            retired_renewal: None,
            headers: vec![block_json(genesis, B256::ZERO, &[])],
            blocks: Vec::new(),
            receipts: HashMap::new(),
            committee,
            journal_path: data_dir.join("tee-service-tx-v1/journal.json"),
        };
        chain.push_header();
        chain
    }

    pub(super) fn advance(
        &mut self,
        timestamp: u64,
        raw: Option<&[u8]>,
    ) -> Option<TxResult<OutbeEvmTypes>> {
        let result = self.latest.advance(timestamp, raw);
        self.push_header();
        if let Some((raw, result)) = raw.zip(result.as_ref()) {
            let tx = keccak256(raw);
            self.receipts
                .insert(tx, receipt(self.latest.tip, tx, result));
        }
        result
    }

    pub(super) fn finalize(&mut self) {
        self.finalized = self.latest.clone();
    }

    fn push_header(&mut self) {
        let header = &self.latest.block.header.inner;
        self.headers.push(block_json(
            self.latest.tip,
            header.parent_hash,
            &header.extra_data,
        ));
        self.blocks
            .push(ConsensusBlock::from_sealed(SealedBlock::seal_slow(
                self.latest.block.clone(),
            )));
    }

    fn header(&self, tag: &str) -> Result<Value, RejoinRpcError> {
        let height = match tag {
            "finalized" => self.finalized.tip.height,
            "latest" => self.latest.tip.height,
            _ => u64::from_str_radix(tag.trim_start_matches("0x"), 16)
                .map_err(|_| RejoinRpcError::Block)?,
        };
        self.headers
            .get(height as usize)
            .cloned()
            .ok_or(RejoinRpcError::Block)
    }

    fn by_selector(&self, selector: &Value) -> Result<&Ledger, RejoinRpcError> {
        match selector.as_str() {
            Some("latest" | "pending") => Ok(&self.latest),
            _ if *selector
                == json!({"blockHash":self.finalized.tip.hash,"requireCanonical":true}) =>
            {
                Ok(&self.finalized)
            }
            _ => Err(RejoinRpcError::Block),
        }
    }

    fn contract(
        &self,
        to: Address,
        data: Bytes,
        selector: &Value,
    ) -> Result<Bytes, RejoinRpcError> {
        self.by_selector(selector)?
            .call(to, data)
            .ok_or(RejoinRpcError::Call(to))
    }

    fn account(&self, account: Address, selector: &Value) -> Result<(u64, U256), RejoinRpcError> {
        if account != Signer::address() {
            return Err(RejoinRpcError::Account(account));
        }
        Ok(self.by_selector(selector)?.nonce_and_balance())
    }

    fn registration_balance(
        &mut self,
        account: Address,
        selector: &Value,
    ) -> Result<U256, RejoinRpcError> {
        if let Some(record) = self.read_record().filter(|record| {
            record.kind == ServiceTxKindV1::Renew
                && record.state == ServiceTxStateV1::RebuildRequired
        }) {
            // Enrollment has not yet overwritten the expired renewal on disk.
            self.retired_renewal = Some(record.tx_hash);
        }
        self.account(account, selector).map(|(_, balance)| balance)
    }

    fn certificate(&self, height: u64) -> Result<Value, RejoinRpcError> {
        let index = height.checked_sub(1).ok_or(RejoinRpcError::Block)?;
        if height > self.finalized.tip.height {
            return Err(RejoinRpcError::Block);
        }
        let block = self
            .blocks
            .get(index as usize)
            .ok_or(RejoinRpcError::Block)?;
        Ok(json!({
            "blockHex": Bytes::from(block.encode().to_vec()),
            "finalizationHex": Bytes::from(self.committee.certify(block.digest(), height)),
        }))
    }

    fn proof(
        &self,
        address: Address,
        keys: [B256; 9],
        selector: &Value,
    ) -> Result<Value, RejoinRpcError> {
        use outbe_primitives::addresses::TEE_REGISTRY_ADDRESS;
        if address != TEE_REGISTRY_ADDRESS || !selector.is_object() {
            return Err(RejoinRpcError::Proof);
        }
        let proof = self.by_selector(selector)?.opening(&self.committee, keys);
        let witness = FinalizedAdmissionWitnessV1::decode_canonical(&proof.admission).unwrap();
        let account = witness.registry_account;
        let nodes = |nodes: Vec<Vec<u8>>| nodes.into_iter().map(Bytes::from).collect::<Vec<_>>();
        let storage: Vec<_> = witness
            .registry_storage
            .into_iter()
            .map(|p| json!({"key":p.key,"value":p.value,"proof":nodes(p.nodes)}))
            .collect();
        Ok(json!({
            "nonce": format!("0x{:x}", account.nonce),
            "balance": account.balance,
            "codeHash": account.code_hash,
            "storageHash": account.storage_root,
            "accountProof": nodes(account.nodes),
            "storageProof": storage,
        }))
    }

    fn submit(&mut self, raw: Bytes) -> B256 {
        let record = self.record();
        assert_eq!(record.raw_signed_tx.as_ref(), raw.as_ref());
        let hash = keccak256(&raw);
        self.sent.push(raw);
        hash
    }

    pub(super) fn record(&self) -> ServiceTxRecordV1 {
        self.read_record()
            .expect("service transaction journal record")
    }

    fn read_record(&self) -> Option<ServiceTxRecordV1> {
        #[derive(serde::Deserialize)]
        struct JournalRecord {
            record: ServiceTxRecordV1,
        }
        match std::fs::read(&self.journal_path) {
            Ok(bytes) => Some(
                serde_json::from_slice::<JournalRecord>(&bytes)
                    .unwrap()
                    .record,
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("read service transaction journal: {error}"),
        }
    }
}

#[derive(serde::Deserialize)]
struct Call {
    to: Address,
    data: Bytes,
}

pub(super) fn methods(chain: std::sync::Arc<Mutex<RejoinChain>>) -> Methods {
    let mut module = RpcModule::new(chain);
    module
        .register_method("eth_chainId", |_, _, _| {
            format!("0x{:x}", crate::TEST_CHAIN_ID)
        })
        .unwrap();
    module
        .register_method("eth_getBlockByNumber", |params, chain, _| {
            let (tag, _full): (String, bool) = params.parse()?;
            chain
                .lock()
                .unwrap()
                .header(&tag)
                .map_err(ErrorObjectOwned::from)
        })
        .unwrap();
    module
        .register_method("eth_call", |params, chain, _| {
            let (call, selector): (Call, Value) = params.parse()?;
            chain
                .lock()
                .unwrap()
                .contract(call.to, call.data, &selector)
                .map_err(ErrorObjectOwned::from)
        })
        .unwrap();
    module
        .register_method("outbe_teeRenewalScheduleV1", |params, chain, _| {
            let args: Vec<Value> = match params.as_str() {
                None => Vec::new(),
                Some(_) => params.parse()?,
            };
            let tip = chain.lock().unwrap().finalized.tip;
            if let Some(value) = args.first().filter(|v| !v.is_null()) {
                let hash: B256 = serde_json::from_value(value.clone()).map_err(|error| {
                    ErrorObjectOwned::owned(-32602, format!("{error}"), None::<()>)
                })?;
                if hash != tip.hash {
                    return Err(ErrorObjectOwned::owned(
                        -32602,
                        format!("unknown schedule checkpoint {hash}"),
                        None::<()>,
                    ));
                }
            }
            Ok::<_, ErrorObjectOwned>(serde_json::to_value(schedule_for(tip)).unwrap())
        })
        .unwrap();
    module
        .register_method("eth_getBalance", |params, chain, _| {
            let (account, selector) = params.parse()?;
            chain
                .lock()
                .unwrap()
                .registration_balance(account, &selector)
                .map_err(ErrorObjectOwned::from)
        })
        .unwrap();
    module
        .register_method("eth_getTransactionCount", |params, chain, _| {
            let (account, selector) = params.parse()?;
            let (nonce, _) = chain.lock().unwrap().account(account, &selector)?;
            Ok::<_, ErrorObjectOwned>(format!("0x{nonce:x}"))
        })
        .unwrap();
    module
        .register_method("eth_sendRawTransaction", |params, chain, _| {
            let raw: Bytes = params.one()?;
            Ok::<_, ErrorObjectOwned>(chain.lock().unwrap().submit(raw))
        })
        .unwrap();
    module
        .register_method("eth_getTransactionReceipt", |params, chain, _| {
            let tx: B256 = params.one()?;
            Ok::<_, ErrorObjectOwned>(chain.lock().unwrap().receipts.get(&tx).cloned())
        })
        .unwrap();
    module
        .register_method("eth_getProof", |params, chain, _| {
            let (address, keys, selector) = params.parse()?;
            chain
                .lock()
                .unwrap()
                .proof(address, keys, &selector)
                .map_err(ErrorObjectOwned::from)
        })
        .unwrap();
    module
        .register_method("outbe_getFinalityProof", |params, chain, _| {
            let height = params.one()?;
            chain
                .lock()
                .unwrap()
                .certificate(height)
                .map_err(ErrorObjectOwned::from)
        })
        .unwrap();
    module.into()
}

fn receipt(at: LifecycleCheckpointV1, tx: B256, result: &TxResult<OutbeEvmTypes>) -> Value {
    let logs: Vec<_> = result
        .logs
        .iter()
        .map(|log| {
            json!({
                "address": log.address,
                "topics": log.data.topics(),
                "data": log.data.data,
                "transactionHash": tx,
                "blockNumber": format!("0x{:x}", at.height),
                "blockHash": at.hash,
                "removed": false,
            })
        })
        .collect();
    json!({
        "transactionHash": tx,
        "blockHash": at.hash,
        "blockNumber": format!("0x{:x}", at.height),
        "status": "0x1",
        "logs": logs,
    })
}
