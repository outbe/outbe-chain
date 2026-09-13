use std::ops::RangeBounds;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, Bytes, B256, U256};
use outbe_metadosis::api::{OcompFinalityAuthorityError, OcompFinalizedIntentAuthority};
use outbe_ocomp_protocol::{
    common::{BoundedBytes, ProofBytes},
    intent::{
        CertifiedParentAccountingMetadataV2, ExpectedFinalizedIntentBindingV1,
        FinalizedIntentProofV1, FinalizedIntentVerificationError, ParentProofKind,
        VerifiedFinalizedIntentV1,
    },
    SchemaLimits,
};
use outbe_primitives::{
    consensus::{ConsensusExecutionBridge, DkgBoundaryArtifact, ReshareResult},
    reshare_artifact::{
        decode_outbe_block_artifacts, encode_consensus_header_artifact,
        encode_outbe_block_artifacts, ConsensusHeaderArtifact, ExecutionSummaryArtifact,
        LateFinalizeCreditsArtifact, OutbeBlockArtifacts, PerBlockCredit,
    },
    tee_genesis_v1::GRAMINE_DIRECT_DEV_CHAIN_ID,
    OutbeHeader,
};
use reth_chainspec::ChainInfo;
use reth_ethereum::chainspec::ChainSpec;
use reth_ethereum::{
    chainspec::MAINNET,
    primitives::{Header, SealedHeader},
};
use reth_provider::{BlockHashReader, BlockIdReader, BlockNumReader, ProviderResult};

use reth_evm::{ConfigureEvm, NextBlockEnvAttributes};
use reth_provider::HeaderProvider;
use reth_rpc_eth_api::helpers::pending_block::BuildPendingEnv;

use super::{
    AccountedParentArtifactProvider, OutbeEvmConfig, OutbeNextBlockEnvAttributes,
    ProviderAnchoredOcompFinalityAuthority, RethAccountedParentArtifactProvider,
};

mod fixtures;
use fixtures::{
    next_block_attrs, test_chain_spec, test_parent, test_parent_with_millis_part, test_summary,
};

mod configuration;

mod parent_artifacts;

mod finality;
