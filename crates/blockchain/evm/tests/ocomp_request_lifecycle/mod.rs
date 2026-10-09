//! Shared constants and adapters for the OCOMP request lifecycle scenarios.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use alloy_consensus::{Block, Header, SignableTransaction, Transaction as _, TxEip1559};
use alloy_eips::{BlockId, BlockNumHash, BlockNumberOrTag};
use alloy_primitives::{address, keccak256, Address, Bytes, TxKind, B256, U256};
use alloy_rpc_types_engine::PayloadId;
use alloy_sol_types::{SolCall, SolEvent};
use commonware_codec::Encode;
use commonware_consensus::{
    simplex::types::{Finalization, Proposal},
    types::{Epoch, Round, View},
};
use commonware_cryptography::{
    bls12381::{
        primitives::{
            ops::{aggregate, keypair, sign_message},
            variant::{MinPk, MinSig, Variant},
        },
        PrivateKey, PublicKey,
    },
    certificate::Signers,
    sha256::Digest as Sha256Digest,
    Signer,
};
use commonware_utils::Participant;
use outbe_compressed_entities::{
    begin_block, end_block, CandidateCacheLimits, CeMdbx, CeTopologyV1, CeWorkConfig,
    CompressedTreeService, EnvironmentIdentity, ExactParentIdentity, FinalizedMarker,
    ACTIVE_COMMITMENT_SCHEME, LOCAL_STORAGE_SCHEMA_VERSION,
};
use outbe_consensus::{
    hybrid::HybridScheme,
    proof::{
        constants::finalize_namespace, hybrid_seed_namespace, CommitteeEntry, CommitteeSnapshot,
        HybridCertificate, VrfProof,
    },
};
use outbe_cycle::schema::Cycle;
use outbe_desis::{AuctionStage, DesisContract};
use outbe_evm::{
    system_tx::{split_system_layout, OcompLifecycleActivation, SystemTxInputV2, SystemTxKind},
    OutbeEvmConfig, OutbeEvmSigner, RethAccountedParentArtifactProvider,
};
use outbe_intex::IntexContract;
use outbe_metadosis::{
    config::poc_schema_limits,
    constants::{
        FORMING_PERIOD_HOURS, LOOKBACK_DELAY_HOURS, OFFERING_PERIOD_HOURS, SECONDS_PER_HOUR,
        WAITING_PERIOD_HOURS,
    },
    genesis::{FreshDevnetGenesisBuilder, GenesisWorldwideDay},
    precompile::IMetadosis,
    test_support::{ForkInstallScenario, ResultVotingScenario},
    WwdDayType,
};
use outbe_nod::NodContract;
use outbe_node::OutbePayloadBuilder;
use outbe_ocomp_protocol::{
    abi::encode_submit_lysis_result_calldata,
    receipts::AggregateActivationReceiptV1,
    state::{ActiveGenerationV1, OcompJobRecordV1, OcompJobStatus, OcompTerminalOutcome},
    vote::OcompVoteAccountabilityV1,
};
use outbe_offchain_data::RuntimeBodyReaders;
use outbe_offchain_storage::{MemoryStorage, StorageReaderHandle};
use outbe_oracle::schema::OracleContract;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    addresses::{
        METADOSIS_ADDRESS, REWARDS_ADDRESS, TRIBUTE_FACTORY_ADDRESS, VALIDATOR_SET_ADDRESS,
    },
    block::{BlockContext, BlockRuntimeContext},
    consensus_metadata::{CertifiedParentAccountingMetadata, ParentParticipationProof},
    projection::ExecutionReadBudget,
    reshare_artifact::{
        decode_outbe_block_artifacts, encode_outbe_block_artifacts, CompressedEntitiesRootArtifact,
        ExecutionSummaryArtifact, OutbeBlockArtifacts,
    },
    storage::{
        direct::DirectStorageProvider, hashmap::HashMapStorageProvider,
        MetadosisMutationPurposeTag, StorageHandle,
    },
    OutbeBlock, OutbeHeader, OutbePayloadAttributes, OutbePrimitives,
};
use outbe_tribute::{TributeContract, TributeData};
use outbe_txpool::OutbeTransactionOrdering;
use outbe_update::{schema::Update, ProtocolVersion};
use outbe_validatorset::{
    committee_snapshot_key, contract::ValidatorSet, read_committee_snapshot,
    write_committee_snapshot, CommitteeSnapshot as StoredCommitteeSnapshot,
};
use reth_basic_payload_builder::{BuildArguments, PayloadBuilder, PayloadConfig};
use reth_chainspec::{ChainInfo, ChainSpec, ChainSpecBuilder, ChainSpecProvider};
use reth_ethereum::{Transaction, TransactionSigned};
use reth_ethereum_payload_builder::EthereumBuilderConfig;
use reth_evm::{execute::Executor as _, ConfigureEvm, RecoveredTx};
use reth_payload_primitives::{BuiltPayload as _, PayloadBuilderError};
use reth_primitives_traits::{
    crypto::secp256k1::sign_message as sign_secp256k1_message, Account, AlloyBlockHeader as _,
    Bytecode, SealedBlock, SealedHeader, SignedTransaction,
};
use reth_provider::{
    test_utils::{ExtendedAccount, MockEthProvider},
    AccountReader, BlockHashReader, BlockIdReader, BlockNumReader, BytecodeReader,
    HashedPostStateProvider, ProviderResult, StateProofProvider, StateProvider, StateProviderBox,
    StateProviderFactory, StateRootProvider, StorageRootProvider,
};
use reth_revm::database::StateProviderDatabase;
use reth_transaction_pool::{
    blobstore::InMemoryBlobStore, noop::MockTransactionValidator, EthPooledTransaction, Pool,
    PoolConfig, PoolTransaction, TransactionOrigin, TransactionPool,
};
use reth_trie::{
    test_utils::{state_root_prehashed, storage_root_prehashed},
    updates::TrieUpdates,
    AccountProof, ExecutionWitnessMode, HashedPostState, HashedStorage, KeccakKeyHasher,
    MultiProof, MultiProofTargets, StorageMultiProof, StorageProof, TrieInput,
};
use revm::Database;

const CHAIN_ID: u64 = 1;
const PARENT_HEIGHT: u64 = 1;
const REQUEST_HEIGHT: u64 = 2;
const BLOCK_GAS_LIMIT: u64 = 30_000_000;
const FINALIZED_EPOCH: u64 = 3;
const FINALIZED_VIEW: u64 = 100;
const PARENT_VIEW: u64 = 99;
const VRF_MATERIAL_VERSION: u64 = 5;
const VALIDATOR_OWNER: Address = address!("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
const SATURATED_USER_TRANSACTION_COUNT: u64 = 40;
const SATURATED_USER_TRANSACTION_GAS: u64 = 1_000_000;
const BURNER_ADDRESS: Address = address!("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB");

mod support;
use support::*;
pub(super) mod carrier_overlay;
pub(super) mod carrier_skip;
pub(super) mod carrier_state_unavailable;
pub(super) mod early_vote;
pub(super) mod expiry;
pub(super) mod near_cap;
pub(super) mod quorum;
pub(super) mod rejection;
mod request;
pub(super) fn reject_forged_prev_randao() {
    request::reject_forged_prev_randao();
}
