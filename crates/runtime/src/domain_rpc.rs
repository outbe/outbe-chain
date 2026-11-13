//! Domain JSON-RPC endpoints merged into the `outbe_*` namespace by the runtime composition root.

use std::sync::Arc;

use alloy_primitives::{Address, Bytes, B256, U256};
use jsonrpsee::core::RpcResult;
use jsonrpsee::proc_macros::rpc;
use outbe_core_common::wwd_entity_id::WwdEntityId;
use outbe_enclave::tribute_source::{TributeSourceSubject, TributeStateSource};
use outbe_primitives::{
    header::OutbeHeader,
    storage::{
        proof::AccountStorageProof,
        readonly::{ReadOnlyStorageProvider, StorageReader},
        StorageHandle,
    },
};
use outbe_system_common::addresses::TRIBUTE_ADDRESS;
use reth_ethereum::primitives::AlloyBlockHeader as _;
use reth_ethereum::storage::{
    HeaderProvider, StateProvider as _, StateProviderBox, StateProviderFactory,
};
use reth_provider::BlockIdReader;
use reth_trie::TrieInput;
use thiserror::Error;

/// Sealed view + modify keys returned by `outbe_deriveKeys` and
/// `outbe_deriveGratisKeys`.
///
/// The enclave derives the account's keys and seals them to the requester's
/// ephemeral X25519 key: `sealed = AEAD(ECDHE(enclaveEphemeral, requesterEphemeral),
/// view_key || modify_key)`. The client recovers `view_key || modify_key` with its
/// ephemeral secret + `enclaveEphemeralPubkey`. Opaque to the node.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GratisKeysSealed {
    pub sealed: Bytes,
    pub nonce: Bytes,
    pub enclave_ephemeral_pubkey: B256,
}

/// Domain methods registered under the `outbe_*` RPC namespace alongside [`outbe_rpc::OutbeApiServer`].
#[rpc(server, namespace = "outbe")]
pub trait OutbeDomainApi {
    /// Returns an encrypted Tribute and exact storage proof at the requested checkpoint.
    #[method(name = "getTributeStateSource")]
    async fn get_tribute_state_source(
        &self,
        subject: TributeSourceSubject,
        tribute_id: WwdEntityId,
    ) -> RpcResult<TributeStateSource>;

    /// Derive the account's confidential view + modify keys for `ledger` (`Gratis`,
    /// `Promis`, `Fidelity`, `Tribute`, or `Nod`) inside the enclave and return them
    /// sealed to `ephemeralPubkey` (a client X25519 public key). This is off-chain key
    /// delivery. It never touches consensus state.
    #[method(name = "deriveKeys")]
    async fn derive_keys(
        &self,
        ledger: outbe_enclave::protocol::Ledger,
        account: Address,
        ephemeral_pubkey: B256,
        signature: Bytes,
    ) -> RpcResult<GratisKeysSealed>;

    /// Deprecated alias for `deriveKeys(Gratis, ...)`, kept for existing Gratis clients.
    #[method(name = "deriveGratisKeys")]
    async fn derive_gratis_keys(
        &self,
        account: Address,
        ephemeral_pubkey: B256,
        signature: Bytes,
    ) -> RpcResult<GratisKeysSealed>;
}

#[derive(Debug, Error)]
enum DomainRpcInternalError {
    #[error("PrivateExecutionUnavailable")]
    PrivateExecutionUnavailable,
    #[error("enclave DeriveAccountKeys failed: {0}")]
    EnclaveDeriveAccountKeysFailed(String),
    #[error("unexpected enclave response: {0}")]
    UnexpectedEnclaveResponse(String),
    #[error("state read failed: {0}")]
    StateReadFailed(String),
    #[error("Tribute source worker failed: {0}")]
    TributeSourceWorkerFailed(String),
}

impl From<DomainRpcInternalError> for outbe_primitives::error::PrecompileError {
    fn from(error: DomainRpcInternalError) -> Self {
        Self::Storage(error.to_string())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Error)]
enum DomainRpcInvalidParamsError {
    #[error("signature must be 65 bytes (r||s||v), got {0}")]
    InvalidSignatureLength(usize),
    #[error("signature recovery failed: {0}")]
    SignatureRecoveryFailed(String),
    #[error("signature signer {recovered} does not control account {account}")]
    SignatureSignerMismatch {
        recovered: Address,
        account: Address,
    },
    #[error("Tribute source day mismatch")]
    TributeSourceDayMismatch,
    #[error("Tribute source chain or day mismatch")]
    TributeSourceChainOrDayMismatch,
    #[error("Tribute source checkpoint is not finalized")]
    TributeSourceCheckpointNotFinalized,
    #[error("Tribute source checkpoint does not match the canonical header")]
    TributeSourceHeaderMismatch,
}

struct RethStateReader<'a> {
    state: &'a StateProviderBox,
}

impl StorageReader for RethStateReader<'_> {
    fn read_storage(&self, address: Address, key: B256) -> outbe_primitives::error::Result<U256> {
        self.state
            .storage(address, key)
            .map(|opt| opt.unwrap_or(U256::ZERO))
            .map_err(|error| DomainRpcInternalError::StateReadFailed(error.to_string()).into())
    }
}

pub(crate) const OUTBE_RPC_NAMESPACE: &str = "outbe";

pub(crate) fn merge_outbe_rpc_modules(
    infra: impl Into<jsonrpsee::Methods>,
    domain: impl Into<jsonrpsee::Methods>,
) -> Result<jsonrpsee::RpcModule<()>, jsonrpsee::core::RegisterMethodError> {
    let mut merged = jsonrpsee::RpcModule::new(());
    merged.merge(infra)?;
    merged.merge(domain)?;
    Ok(merged)
}

pub(crate) fn register_outbe_rpc_if_configured(
    modules: &mut reth_ethereum::rpc::builder::TransportRpcModules,
    infra: impl Into<jsonrpsee::Methods>,
    domain: impl Into<jsonrpsee::Methods>,
) -> Result<(), jsonrpsee::core::RegisterMethodError> {
    let merged = merge_outbe_rpc_modules(infra, domain)?;
    modules.merge_if_module_configured(
        crate::RethRpcModule::Other(OUTBE_RPC_NAMESPACE.to_owned()),
        merged,
    )
}

type AccountQueryFn =
    fn(
        &outbe_enclave::protocol::EnclaveRequest,
    ) -> Option<Result<outbe_enclave::protocol::EnclaveResponse, outbe_tee::TransportError>>;

/// Runtime handler serving domain RPC methods in the `outbe_*` namespace.
#[derive(Clone)]
pub(crate) struct OutbeDomainRpcHandler<P> {
    provider: Arc<P>,
    chain_identity: Option<(u64, B256)>,
    account_query: AccountQueryFn,
}

impl<P> OutbeDomainRpcHandler<P> {
    #[must_use]
    pub(crate) fn new(provider: Arc<P>, chain_identity: (u64, B256)) -> Self {
        Self {
            provider,
            chain_identity: Some(chain_identity),
            account_query: outbe_enclave::try_domain_account_query,
        }
    }

    #[cfg(test)]
    #[must_use]
    fn with_account_query(mut self, account_query: AccountQueryFn) -> Self {
        self.account_query = account_query;
        self
    }

    pub(crate) async fn derive_keys_checked(
        &self,
        ledger: outbe_enclave::protocol::Ledger,
        account: Address,
        ephemeral_pubkey: B256,
        signature: Bytes,
    ) -> RpcResult<GratisKeysSealed> {
        use outbe_enclave::protocol::{EnclaveRequest, EnclaveResponse};

        let sig65: [u8; 65] = signature.as_ref().try_into().map_err(|_| {
            invalid_params_err(DomainRpcInvalidParamsError::InvalidSignatureLength(
                signature.len(),
            ))
        })?;
        let prehash = outbe_enclave::protocol::eip191_hash(
            &outbe_enclave::protocol::derive_account_keys_message(
                ledger,
                account,
                ephemeral_pubkey,
            ),
        );
        let recovered = outbe_primitives::tee_signatures::recover_signer(&prehash, &sig65)
            .map_err(|error| {
                invalid_params_err(DomainRpcInvalidParamsError::SignatureRecoveryFailed(
                    error.to_string(),
                ))
            })?;
        if recovered != account {
            return Err(invalid_params_err(
                DomainRpcInvalidParamsError::SignatureSignerMismatch { recovered, account },
            ));
        }

        let response = (self.account_query)(&EnclaveRequest::DeriveAccountKeys {
            ledger,
            account,
            requester_ephemeral_pubkey: ephemeral_pubkey.0,
            owner_sig: sig65.to_vec(),
        })
        .ok_or_else(|| internal_err(DomainRpcInternalError::PrivateExecutionUnavailable))?
        .map_err(|error| {
            internal_err(DomainRpcInternalError::EnclaveDeriveAccountKeysFailed(
                error.to_string(),
            ))
        })?;

        match response {
            EnclaveResponse::AccountKeysSealed {
                sealed,
                nonce,
                enclave_ephemeral_pubkey,
                ..
            } => Ok(GratisKeysSealed {
                sealed: sealed.into(),
                nonce: nonce.to_vec().into(),
                enclave_ephemeral_pubkey: B256::from(enclave_ephemeral_pubkey),
            }),
            other => Err(internal_err(
                DomainRpcInternalError::UnexpectedEnclaveResponse(format!("{other:?}")),
            )),
        }
    }

    pub(crate) async fn derive_gratis_keys_checked(
        &self,
        account: Address,
        ephemeral_pubkey: B256,
        signature: Bytes,
    ) -> RpcResult<GratisKeysSealed> {
        self.derive_keys_checked(
            outbe_enclave::protocol::Ledger::Gratis,
            account,
            ephemeral_pubkey,
            signature,
        )
        .await
    }
}

#[jsonrpsee::core::async_trait]
impl<P> OutbeDomainApiServer for OutbeDomainRpcHandler<P>
where
    P: StateProviderFactory
        + HeaderProvider<Header = OutbeHeader>
        + BlockIdReader
        + Send
        + Sync
        + 'static,
{
    async fn get_tribute_state_source(
        &self,
        subject: TributeSourceSubject,
        tribute_id: WwdEntityId,
    ) -> RpcResult<TributeStateSource> {
        let provider = Arc::clone(&self.provider);
        let chain_identity = self.chain_identity;
        tokio::task::spawn_blocking(move || {
            serve_tribute_state_source(&*provider, chain_identity, subject, tribute_id)
        })
        .await
        .map_err(|error| {
            internal_err(DomainRpcInternalError::TributeSourceWorkerFailed(
                error.to_string(),
            ))
        })?
    }

    async fn derive_keys(
        &self,
        ledger: outbe_enclave::protocol::Ledger,
        account: Address,
        ephemeral_pubkey: B256,
        signature: Bytes,
    ) -> RpcResult<GratisKeysSealed> {
        self.derive_keys_checked(ledger, account, ephemeral_pubkey, signature)
            .await
    }

    async fn derive_gratis_keys(
        &self,
        account: Address,
        ephemeral_pubkey: B256,
        signature: Bytes,
    ) -> RpcResult<GratisKeysSealed> {
        self.derive_gratis_keys_checked(account, ephemeral_pubkey, signature)
            .await
    }
}

fn serve_tribute_state_source<P>(
    provider: &P,
    chain_identity: Option<(u64, B256)>,
    subject: TributeSourceSubject,
    tribute_id: WwdEntityId,
) -> RpcResult<TributeStateSource>
where
    P: StateProviderFactory + HeaderProvider<Header = OutbeHeader> + BlockIdReader,
{
    if tribute_id.worldwide_day() != subject.worldwide_day {
        return Err(invalid_params_err(
            DomainRpcInvalidParamsError::TributeSourceDayMismatch,
        ));
    }
    let (chain_id, genesis_hash, state) = checkpoint_state(provider, chain_identity, subject)?;
    let reader = RethStateReader { state: &state };
    let mut readonly =
        ReadOnlyStorageProvider::new_with_chain_identity(reader, chain_id, genesis_hash);
    let (tribute, day_index, expected_words) = StorageHandle::enter(&mut readonly, |storage| {
        outbe_tribute::state_proof::read_tribute_source_words(storage, &subject, tribute_id)
    })
    .map_err(internal_err)?;
    let slots = expected_words
        .iter()
        .map(|(slot, _)| *slot)
        .collect::<Vec<_>>();
    let proof = state
        .proof(TrieInput::default(), TRIBUTE_ADDRESS, &slots)
        .map_err(internal_err)?;
    let source = TributeStateSource {
        tribute,
        day_index,
        expected_words,
        proof: AccountStorageProof::try_from(&proof).map_err(internal_err)?,
    };
    source
        .verify(
            &subject,
            source.binding().map_err(internal_err)?,
            outbe_enclave::tribute_source::proof_limits(),
        )
        .map_err(internal_err)?;
    Ok(source)
}

fn checkpoint_state<P>(
    provider: &P,
    chain_identity: Option<(u64, B256)>,
    subject: TributeSourceSubject,
) -> RpcResult<(u64, B256, StateProviderBox)>
where
    P: StateProviderFactory + HeaderProvider<Header = OutbeHeader> + BlockIdReader,
{
    let (chain_id, genesis_hash) = chain_identity
        .filter(|(chain_id, genesis_hash)| *chain_id != 0 && !genesis_hash.is_zero())
        .ok_or_else(|| internal_err("immutable chain identity is not configured"))?;
    if chain_id != subject.chain_id || subject.worldwide_day.value() == 0 {
        return Err(invalid_params_err(
            DomainRpcInvalidParamsError::TributeSourceChainOrDayMismatch,
        ));
    }
    let checkpoint = subject.checkpoint;
    let finalized = provider
        .finalized_block_num_hash()
        .map_err(internal_err)?
        .ok_or_else(|| internal_err("finalized state unavailable"))?;
    if checkpoint.block_number == 0 || checkpoint.block_number > finalized.number {
        return Err(invalid_params_err(
            DomainRpcInvalidParamsError::TributeSourceCheckpointNotFinalized,
        ));
    }
    let header = provider
        .sealed_header(checkpoint.block_number)
        .map_err(internal_err)?
        .ok_or_else(|| internal_err("Tribute source header unavailable"))?;
    if header.hash() != checkpoint.block_hash || header.state_root() != checkpoint.state_root {
        return Err(invalid_params_err(
            DomainRpcInvalidParamsError::TributeSourceHeaderMismatch,
        ));
    }
    let state = provider
        .state_by_block_hash(checkpoint.block_hash)
        .map_err(internal_err)?;
    Ok((chain_id, genesis_hash, state))
}

fn internal_err(msg: impl std::fmt::Display) -> jsonrpsee::types::ErrorObject<'static> {
    jsonrpsee::types::ErrorObject::owned(
        jsonrpsee::types::error::INTERNAL_ERROR_CODE,
        msg.to_string(),
        None::<()>,
    )
}

fn invalid_params_err(msg: impl std::fmt::Display) -> jsonrpsee::types::ErrorObject<'static> {
    jsonrpsee::types::ErrorObject::owned(
        jsonrpsee::types::error::INVALID_PARAMS_CODE,
        msg.to_string(),
        None::<()>,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_enclave::protocol::{derive_account_keys_message, eip191_hash, Ledger};
    use outbe_primitives::signer::OutbeEvmSigner;
    use outbe_rpc::OutbeApiServer as _;
    use reth_provider::test_utils::NoopProvider;

    fn sample_recoverable_sig(prehash: &B256, exclude: Option<Address>) -> ([u8; 65], Address) {
        (1u8..=16)
            .find_map(|r_byte| {
                let mut sig = [0u8; 65];
                sig[31] = r_byte;
                sig[63] = 1;
                sig[64] = 27;
                outbe_primitives::tee_signatures::recover_signer(prehash, &sig)
                    .ok()
                    .filter(|addr| exclude != Some(*addr))
                    .map(|addr| (sig, addr))
            })
            .expect("should find a recoverable secp256k1 point")
    }

    #[tokio::test]
    async fn derive_keys_and_gratis_alias_reject_invalid_and_mismatched_signatures() {
        let handler = OutbeDomainRpcHandler::new(Arc::new(()), (1, B256::repeat_byte(0x11)));
        let account = Address::repeat_byte(0x11);
        let ephemeral_pubkey = B256::repeat_byte(0x22);

        let short_err = handler
            .derive_keys_checked(
                Ledger::Gratis,
                account,
                ephemeral_pubkey,
                Bytes::from_static(&[1, 2, 3]),
            )
            .await
            .unwrap_err();
        assert_eq!(
            short_err.code(),
            jsonrpsee::types::error::INVALID_PARAMS_CODE
        );
        assert_eq!(
            short_err.message(),
            "signature must be 65 bytes (r||s||v), got 3"
        );

        let malformed_err = handler
            .derive_gratis_keys_checked(account, ephemeral_pubkey, Bytes::from([0xff; 65]))
            .await
            .unwrap_err();
        assert_eq!(
            malformed_err.code(),
            jsonrpsee::types::error::INVALID_PARAMS_CODE
        );
        assert!(malformed_err
            .message()
            .starts_with("signature recovery failed:"));

        for ledger in [
            Ledger::Gratis,
            Ledger::Promis,
            Ledger::Fidelity,
            Ledger::Tribute,
            Ledger::Nod,
        ] {
            let prehash = eip191_hash(&derive_account_keys_message(
                ledger,
                account,
                ephemeral_pubkey,
            ));
            let (foreign_sig, recovered) = sample_recoverable_sig(&prehash, Some(account));

            let err = handler
                .derive_keys_checked(
                    ledger,
                    account,
                    ephemeral_pubkey,
                    Bytes::copy_from_slice(&foreign_sig),
                )
                .await
                .unwrap_err();
            assert_eq!(err.code(), jsonrpsee::types::error::INVALID_PARAMS_CODE);
            assert_eq!(
                err.message(),
                format!("signature signer {recovered} does not control account {account}")
            );
        }

        let gratis_prehash = eip191_hash(&derive_account_keys_message(
            Ledger::Gratis,
            account,
            ephemeral_pubkey,
        ));
        let (foreign_sig, recovered) = sample_recoverable_sig(&gratis_prehash, Some(account));

        let err = handler
            .derive_gratis_keys_checked(
                account,
                ephemeral_pubkey,
                Bytes::copy_from_slice(&foreign_sig),
            )
            .await
            .unwrap_err();
        assert_eq!(err.code(), jsonrpsee::types::error::INVALID_PARAMS_CODE);
        assert_eq!(
            err.message(),
            format!("signature signer {recovered} does not control account {account}")
        );
    }

    #[tokio::test]
    async fn derive_keys_rejects_cross_domain_signature_replay() {
        let handler = OutbeDomainRpcHandler::new(Arc::new(()), (1, B256::repeat_byte(0x11)))
            .with_account_query(|_| None);
        let signer = OutbeEvmSigner::from_secret_bytes([0x42; 32]).expect("signer");
        let owner = signer.address();
        let ephemeral_pubkey = B256::repeat_byte(0x22);

        let gratis_prehash = eip191_hash(&derive_account_keys_message(
            Ledger::Gratis,
            owner,
            ephemeral_pubkey,
        ));
        let gratis_sig = signer.sign_hash(&gratis_prehash).expect("sign gratis");

        let same_domain_err = handler
            .derive_keys_checked(
                Ledger::Gratis,
                owner,
                ephemeral_pubkey,
                Bytes::copy_from_slice(&gratis_sig),
            )
            .await
            .unwrap_err();
        assert_eq!(
            same_domain_err.code(),
            jsonrpsee::types::error::INTERNAL_ERROR_CODE
        );

        for target_ledger in [
            Ledger::Promis,
            Ledger::Fidelity,
            Ledger::Tribute,
            Ledger::Nod,
        ] {
            let err = handler
                .derive_keys_checked(
                    target_ledger,
                    owner,
                    ephemeral_pubkey,
                    Bytes::copy_from_slice(&gratis_sig),
                )
                .await
                .unwrap_err();
            assert_eq!(err.code(), jsonrpsee::types::error::INVALID_PARAMS_CODE);
            assert!(err
                .message()
                .contains(&format!("does not control account {owner}")));
        }

        let tribute_prehash = eip191_hash(&derive_account_keys_message(
            Ledger::Tribute,
            owner,
            ephemeral_pubkey,
        ));
        let tribute_sig = signer.sign_hash(&tribute_prehash).expect("sign tribute");
        let cross_err = handler
            .derive_keys_checked(
                Ledger::Nod,
                owner,
                ephemeral_pubkey,
                Bytes::copy_from_slice(&tribute_sig),
            )
            .await
            .unwrap_err();
        assert_eq!(
            cross_err.code(),
            jsonrpsee::types::error::INVALID_PARAMS_CODE
        );
        assert!(cross_err
            .message()
            .contains(&format!("does not control account {owner}")));
    }

    #[test]
    fn merged_outbe_rpc_module_exposes_infra_and_domain_methods() {
        let provider: Arc<
            NoopProvider<reth_chainspec::ChainSpec<OutbeHeader>, outbe_primitives::OutbePrimitives>,
        > = Arc::new(NoopProvider::new(Arc::new(
            reth_chainspec::ChainSpec::default(),
        )));
        let runtime = Arc::new(crate::OutbeRuntime::new());
        let infra = outbe_rpc::OutbeApiHandler::new(
            Arc::clone(&provider),
            runtime as Arc<dyn outbe_rpc::state_reader::OutbeRpcStateReader>,
        );
        let domain = OutbeDomainRpcHandler::new(provider, (1, B256::repeat_byte(0xAA)));
        let rpc_module = merge_outbe_rpc_modules(infra.into_rpc(), domain.into_rpc())
            .expect("infra and domain outbe_* RPC modules must merge cleanly");

        let names: std::collections::BTreeSet<&str> = rpc_module.method_names().collect();
        let expected: std::collections::BTreeSet<&str> = [
            "outbe_consensusStatus",
            "outbe_deriveGratisKeys",
            "outbe_deriveKeys",
            "outbe_getConsensusBlock",
            "outbe_getEmissionInfo",
            "outbe_getEpochInfo",
            "outbe_getFinalityProof",
            "outbe_getFinalization",
            "outbe_getParticipation",
            "outbe_getSlashConfig",
            "outbe_getSlashInfo",
            "outbe_getStake",
            "outbe_getTributeStateSource",
            "outbe_getValidator",
            "outbe_getValidators",
            "outbe_getVrfSeed",
            "outbe_radiclePeers",
            "outbe_radicleRepositories",
            "outbe_radicleStatus",
            "outbe_syncStatus",
            "outbe_teeRenewalScheduleV1",
            "outbe_upgradeKeyV1",
        ]
        .into_iter()
        .collect();
        assert_eq!(expected.len(), 22);
        assert_eq!(names, expected);
    }

    #[test]
    fn disabled_outbe_namespace_registers_zero_methods() {
        let provider: Arc<
            NoopProvider<reth_chainspec::ChainSpec<OutbeHeader>, outbe_primitives::OutbePrimitives>,
        > = Arc::new(NoopProvider::new(Arc::new(
            reth_chainspec::ChainSpec::default(),
        )));
        let runtime = Arc::new(crate::OutbeRuntime::new());
        let infra = outbe_rpc::OutbeApiHandler::new(
            Arc::clone(&provider),
            runtime as Arc<dyn outbe_rpc::state_reader::OutbeRpcStateReader>,
        );
        let domain = OutbeDomainRpcHandler::new(provider, (1, B256::repeat_byte(0xAA)));

        let mut unconfigured = reth_ethereum::rpc::builder::TransportRpcModules::default();
        register_outbe_rpc_if_configured(&mut unconfigured, infra.into_rpc(), domain.into_rpc())
            .expect("unconfigured namespace must not error");
        assert!(unconfigured
            .methods_by_module(crate::RethRpcModule::Other(OUTBE_RPC_NAMESPACE.to_owned()))
            .method_names()
            .next()
            .is_none());
    }

    #[tokio::test]
    async fn derive_keys_and_gratis_alias_route_over_account_query_lane_end_to_end() {
        use outbe_enclave::protocol::{EnclaveRequest as DomReq, EnclaveResponse as DomResp};
        use outbe_tee::protocol::{EnclaveRequest as WireReq, EnclaveResponse as WireResp};

        let lanes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let lanes_loop = Arc::clone(&lanes);
        let server = outbe_tee::test_utils::MockDevEnclaveServer::spawn([0xAA; 32], move |req| {
            let WireReq::Payload { lane, payload } = req else {
                return WireResp::Ack;
            };
            lanes_loop.lock().unwrap().push(lane);
            let DomReq::DeriveAccountKeys { account, .. } =
                DomReq::decode_wire_canonical(&payload).unwrap()
            else {
                panic!("unexpected domain request on account query");
            };
            DomResp::AccountKeysSealed {
                account,
                sealed: vec![0x5A; 80],
                nonce: [0x6B; 12],
                enclave_ephemeral_pubkey: [0x7C; 32],
            }
            .into_wire_response()
            .unwrap()
        });

        let client = outbe_tee::EnclaveClient::connect_endpoint(&server.endpoint).unwrap();
        outbe_tee::install_enclave_client(client, server.endpoint.clone()).expect("install once");

        let provider: Arc<
            NoopProvider<reth_chainspec::ChainSpec<OutbeHeader>, outbe_primitives::OutbePrimitives>,
        > = Arc::new(NoopProvider::new(Arc::new(
            reth_chainspec::ChainSpec::default(),
        )));
        let domain = OutbeDomainRpcHandler::new(provider, (1, B256::repeat_byte(0xAA))).into_rpc();

        let signer = OutbeEvmSigner::from_secret_bytes([0x55; 32]).unwrap();
        let owner = signer.address();
        let eph = B256::repeat_byte(0x33);

        let nod_hash = eip191_hash(&derive_account_keys_message(Ledger::Nod, owner, eph));
        let nod_sig = Bytes::copy_from_slice(&signer.sign_hash(&nod_hash).unwrap());
        let nod_sealed: GratisKeysSealed = domain
            .call("outbe_deriveKeys", (Ledger::Nod, owner, eph, nod_sig))
            .await
            .expect("outbe_deriveKeys over AccountQuery");
        assert_eq!(nod_sealed.sealed, Bytes::from(vec![0x5A; 80]));
        assert_eq!(nod_sealed.nonce, Bytes::from(vec![0x6B; 12]));
        assert_eq!(nod_sealed.enclave_ephemeral_pubkey, B256::repeat_byte(0x7C));

        let gratis_hash = eip191_hash(&derive_account_keys_message(Ledger::Gratis, owner, eph));
        let gratis_sig = Bytes::copy_from_slice(&signer.sign_hash(&gratis_hash).unwrap());
        let gratis_sealed: GratisKeysSealed = domain
            .call("outbe_deriveGratisKeys", (owner, eph, gratis_sig))
            .await
            .expect("outbe_deriveGratisKeys over AccountQuery");
        assert_eq!(gratis_sealed.sealed, Bytes::from(vec![0x5A; 80]));

        assert_eq!(
            *lanes.lock().unwrap(),
            vec![
                outbe_tee::protocol::PayloadLane::AccountQuery,
                outbe_tee::protocol::PayloadLane::AccountQuery,
            ]
        );
    }
}
