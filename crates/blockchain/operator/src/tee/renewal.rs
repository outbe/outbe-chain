//! Manual, crash-replay-safe TEE lease-renewal reducer.

#[cfg(test)]
use crate::tx::UnsignedRelayTransactionV1;
mod identity;
mod lifecycle;
mod preparation;
mod submission;

use lifecycle::run_locked_renewal;
use preparation::RenewalPreparation;

#[cfg(test)]
use identity::*;
#[cfg(test)]
use lifecycle::*;
#[cfg(test)]
use preparation::*;
#[cfg(test)]
use submission::*;

use std::path::PathBuf;

use alloy_primitives::{keccak256, B256, U256};
use alloy_sol_types::SolCall as _;
use eyre::{Result, WrapErr as _};
use outbe_primitives::{
    addresses::TEE_REGISTRY_ADDRESS,
    tee_attestation_v1::{
        AttestationEvidenceV1, AttestationMode, AttestationOperationV1, DcapEvidenceV1,
        EnclaveInitializationManifestV1, GramineDirectEvidenceV1, RegistrationIntentV1,
        RegistryMutatorV1, TeeRegistryGasScheduleV1,
    },
    tee_registry_abi_v1::ITeeRegistryV1,
};
use outbe_tee::{
    acquire_dcap_collateral_v1, dcap_collateral_validity_window_v1,
    dcap_protocol::dcap_evidence_hash_v1, AuthorizedEnclaveClient, GeneratedDcapQuoteV1,
};

use crate::{
    errors::RenewalError,
    rpc::{RelayPreparationRpc, RelayRpc, RenewalRpc, TransactionReceiptRpc},
    tx::{buffered_gas_price, RelaySignerV1},
};

use super::{
    registry::{
        read_finalized_bound_renewal_view_v1, FinalizedRenewalChainViewV1, NodeBindingSelectorV1,
        RenewalBindingV1,
    },
    renewal_journal::{
        PreparedRenewalV1, RenewalJournalGuard, RenewalJournalSnapshotV1, RenewalJournalStateV1,
    },
};

pub trait RenewalEnclaveV1 {
    fn generate_dcap_quote(
        &mut self,
        intent: &RegistrationIntentV1,
    ) -> Result<GeneratedDcapQuoteV1>;

    fn sign_registration_intent_dev_v1(
        &mut self,
        _intent: &RegistrationIntentV1,
    ) -> Result<[u8; 64]> {
        Err(RenewalError::DirectDevSigningUnsupported.into())
    }
}

impl RenewalEnclaveV1 for AuthorizedEnclaveClient {
    fn generate_dcap_quote(
        &mut self,
        intent: &RegistrationIntentV1,
    ) -> Result<GeneratedDcapQuoteV1> {
        Ok(AuthorizedEnclaveClient::generate_dcap_quote(self, intent)?)
    }

    fn sign_registration_intent_dev_v1(
        &mut self,
        intent: &RegistrationIntentV1,
    ) -> Result<[u8; 64]> {
        Ok(AuthorizedEnclaveClient::sign_registration_intent_dev_v1(
            self, intent,
        )?)
    }
}

pub trait RenewalNodeSignerV1 {
    fn sign_node_hash(&self, hash: B256) -> Result<[u8; 65]>;
}

impl<F> RenewalNodeSignerV1 for F
where
    F: Fn(B256) -> Result<[u8; 65]>,
{
    fn sign_node_hash(&self, hash: B256) -> Result<[u8; 65]> {
        self(hash)
    }
}

#[derive(Clone, Debug)]
pub struct RenewalServiceConfigV1 {
    pub node_data_dir: PathBuf,
    pub selector: NodeBindingSelectorV1,
    pub manifest: EnclaveInitializationManifestV1,
}

/// RPC, enclave and signing capabilities used by one locked renewal attempt.
/// The configured node identity and storage location are supplied separately.
pub struct RenewalServicesV1<'a, R, E, N> {
    pub rpc: &'a R,
    pub evm_signer: &'a RelaySignerV1,
    pub enclave: &'a mut E,
    pub node_signer: &'a N,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenewalOutcomeV1 {
    NotDue {
        finalized_height: u64,
        opens_at_timestamp: u64,
    },
    Submitted {
        transaction_hash: B256,
        replayed: bool,
    },
    Finalized {
        finalized_height: u64,
        valid_until: u64,
    },
    Abandoned {
        finalized_height: u64,
        reason: String,
    },
}

pub async fn run_renewal_once_v1<
    R: RenewalRpc + Sync,
    E: RenewalEnclaveV1,
    N: RenewalNodeSignerV1,
>(
    services: RenewalServicesV1<'_, R, E, N>,
    config: &RenewalServiceConfigV1,
) -> Result<RenewalOutcomeV1> {
    let mut preparation = RenewalPreparation {
        rpc: services.rpc,
        evm_signer: services.evm_signer,
        enclave: services.enclave,
        node_signer: services.node_signer,
        config,
    };
    run_locked_renewal(&mut preparation).await
}

#[cfg(test)]
mod tests;
