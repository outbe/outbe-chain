//! Domain JSON-RPC endpoints merged into the `outbe_*` namespace by the runtime composition root.

use alloy_primitives::{Address, Bytes, B256};
use jsonrpsee::core::RpcResult;
use jsonrpsee::proc_macros::rpc;
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
    /// Derive the account's confidential view + modify keys for `ledger` (`Gratis`,
    /// `Promis`, `Fidelity`, `Tribute`, or `Nod`) inside the enclave and return them
    /// sealed to `ephemeralPubkey` (a client X25519 public key). This is off-chain key
    /// delivery. It never touches consensus state.
    #[method(name = "deriveKeys")]
    async fn derive_keys(
        &self,
        ledger: outbe_core_common::confidential_tee::Ledger,
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
    EnclaveDeriveAccountKeysFailed(#[source] outbe_tee::TransportError),
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

type AccountQueryFn = fn(
    outbe_core_common::confidential_tee::Ledger,
    Address,
    [u8; 32],
    [u8; 65],
) -> Option<
    Result<outbe_tee::resident::SealedAccountKeysExport, outbe_tee::TransportError>,
>;

/// Runtime handler serving domain RPC methods in the `outbe_*` namespace.
#[derive(Clone)]
pub(crate) struct OutbeDomainRpcHandler {
    account_query: AccountQueryFn,
    authority_watch: Option<outbe_tee::authority::TeeAuthorityWatch>,
}

impl OutbeDomainRpcHandler {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            account_query: outbe_core_common::confidential_tee::try_derive_sealed_account_keys,
            authority_watch: None,
        }
    }

    #[cfg(test)]
    #[must_use]
    fn with_account_query(mut self, account_query: AccountQueryFn) -> Self {
        self.account_query = account_query;
        self
    }

    pub(crate) fn with_authority_watch(
        mut self,
        watch: outbe_tee::authority::TeeAuthorityWatch,
    ) -> Self {
        self.authority_watch = Some(watch);
        self
    }

    pub(crate) async fn derive_keys_checked(
        &self,
        ledger: outbe_core_common::confidential_tee::Ledger,
        account: Address,
        ephemeral_pubkey: B256,
        signature: Bytes,
    ) -> RpcResult<GratisKeysSealed> {
        require_private_authority(self.authority_watch.as_ref())?;

        let sig65: [u8; 65] = signature.as_ref().try_into().map_err(|_| {
            invalid_params_err(DomainRpcInvalidParamsError::InvalidSignatureLength(
                signature.len(),
            ))
        })?;
        let prehash = outbe_core_common::confidential_tee::eip191_hash(
            &outbe_core_common::confidential_tee::derive_account_keys_message(
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

        require_private_authority(self.authority_watch.as_ref())?;
        let sealed = (self.account_query)(ledger, account, ephemeral_pubkey.0, sig65)
            .ok_or_else(|| internal_err(DomainRpcInternalError::PrivateExecutionUnavailable))?
            .map_err(|error| {
                internal_err(DomainRpcInternalError::EnclaveDeriveAccountKeysFailed(
                    error,
                ))
            })?;
        Ok(GratisKeysSealed {
            sealed: sealed.sealed.into(),
            nonce: sealed.nonce.to_vec().into(),
            enclave_ephemeral_pubkey: B256::from(sealed.enclave_ephemeral_pubkey),
        })
    }

    pub(crate) async fn derive_gratis_keys_checked(
        &self,
        account: Address,
        ephemeral_pubkey: B256,
        signature: Bytes,
    ) -> RpcResult<GratisKeysSealed> {
        self.derive_keys_checked(
            outbe_core_common::confidential_tee::Ledger::Gratis,
            account,
            ephemeral_pubkey,
            signature,
        )
        .await
    }
}

#[jsonrpsee::core::async_trait]
impl OutbeDomainApiServer for OutbeDomainRpcHandler {
    async fn derive_keys(
        &self,
        ledger: outbe_core_common::confidential_tee::Ledger,
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

#[cfg(feature = "test-hooks")]
pub async fn account_query_through_authority_gate(
    watch: Option<outbe_tee::authority::TeeAuthorityWatch>,
    ledger: outbe_core_common::confidential_tee::Ledger,
    account: Address,
    ephemeral_pubkey: B256,
    signature: Bytes,
) -> RpcResult<GratisKeysSealed> {
    let mut handler = OutbeDomainRpcHandler::new();
    if let Some(watch) = watch {
        handler = handler.with_authority_watch(watch);
    }
    handler
        .derive_keys_checked(ledger, account, ephemeral_pubkey, signature)
        .await
}

fn require_private_authority(
    watch: Option<&outbe_tee::authority::TeeAuthorityWatch>,
) -> RpcResult<()> {
    let state = match watch {
        Some(watch) => watch.ensure_current(),
        None => outbe_tee::authority::tee_authority_watch().map_or_else(
            outbe_tee::authority::TeeAuthorityState::default,
            |installed| installed.ensure_current(),
        ),
    };
    if state.is_authority_open() && state.has_verified_resident_key() {
        Ok(())
    } else {
        Err(internal_err(
            DomainRpcInternalError::PrivateExecutionUnavailable,
        ))
    }
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
    use outbe_core_common::confidential_tee::{derive_account_keys_message, eip191_hash, Ledger};
    use outbe_primitives::header::OutbeHeader;
    use outbe_primitives::signer::OutbeEvmSigner;
    use outbe_rpc::OutbeApiServer as _;
    use reth_provider::test_utils::NoopProvider;
    use std::sync::Arc;

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
        let authority = outbe_tee::authority::TeeAuthorityChannel::new(
            outbe_tee::authority::TeeAuthorityState::Armed {
                enclave_id: B256::repeat_byte(7),
                valid_until: 123,
            },
        );
        let handler = OutbeDomainRpcHandler::new().with_authority_watch(authority.subscribe());
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
        let authority = outbe_tee::authority::TeeAuthorityChannel::new(
            outbe_tee::authority::TeeAuthorityState::Armed {
                enclave_id: B256::repeat_byte(7),
                valid_until: 123,
            },
        );
        let handler = OutbeDomainRpcHandler::new()
            .with_authority_watch(authority.subscribe())
            .with_account_query(|_, _, _, _| None);
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
        let domain = OutbeDomainRpcHandler::new();
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
        let domain = OutbeDomainRpcHandler::new();

        let mut unconfigured = reth_ethereum::rpc::builder::TransportRpcModules::default();
        register_outbe_rpc_if_configured(&mut unconfigured, infra.into_rpc(), domain.into_rpc())
            .expect("unconfigured namespace must not error");
        assert!(unconfigured
            .methods_by_module(crate::RethRpcModule::Other(OUTBE_RPC_NAMESPACE.to_owned()))
            .method_names()
            .next()
            .is_none());
    }
}

#[cfg(test)]
mod authority_tests;
