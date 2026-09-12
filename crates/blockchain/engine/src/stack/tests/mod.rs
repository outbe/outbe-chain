use super::*;
use alloy_primitives::{Address, Bytes, B256};
use commonware_actor::{Feedback, Unreliable};
use commonware_consensus::{
    marshal::{self, core::Buffer, resolver::handler, Start, Update},
    simplex::{
        elector::{Config as _, Elector as _, RoundRobin},
        types::{Activity, Finalization, Finalize, Proposal, Subject},
        Config as SimplexConfig, Engine as SimplexEngine, Floor, ForwardPolicy,
    },
    types::{Epoch, FixedEpocher, Height, Round, View, ViewDelta},
    Reporter,
};
use commonware_cryptography::bls12381::{primitives::variant::MinSig, PrivateKey};
use commonware_cryptography::certificate::{Scheme as _, Verifier as _};
use commonware_cryptography::sha256::Digest as Sha256Digest;
use commonware_cryptography::{Hasher as _, Sha256};
use commonware_math::algebra::Random;
use commonware_p2p::{Blocker, CheckedSender, LimitedSender, Message, Receiver, Recipients};
use commonware_parallel::Sequential;
use commonware_resolver::Resolver;
use commonware_resolver::TargetedResolver;
use commonware_runtime::{
    buffer::paged::CacheRef, tokio as commonware_tokio, IoBufs, Runner as _, Supervisor as _,
};
use commonware_storage::archive::immutable;
use commonware_utils::{
    acknowledgement::Acknowledgement,
    channel::oneshot,
    ordered::{Quorum as _, Set},
    vec::NonEmptyVec,
    NZUsize,
};
use futures::FutureExt as _;
use outbe_consensus::{
    block::ConsensusBlock,
    bls::bootstrap_dkg,
    committee_provider::CommitteeProvider,
    hybrid::{HybridScheme, HybridSchemeProvider, VrfMaterialProvider},
    reporter::ReporterContinuity,
    test_harness::{mock_genesis, MockAutomaton, MockRelay, MockReporter},
};
use outbe_primitives::OutbeHeader;
use outbe_radicle::integration::{RadicleStatusChannel, RadicleVotingGate, RadicleVotingGateError};
use reth_ethereum::{
    primitives::{Header, SealedBlock, SealedHeader},
    Block,
};
use reth_provider::ProviderResult;
use std::{
    collections::BTreeMap,
    convert::Infallible,
    marker::PhantomData,
    num::{NonZeroU16, NonZeroU64, NonZeroUsize},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc, Barrier, Mutex as StdMutex,
    },
    time::{Duration, SystemTime},
};

mod configuration;
mod dkg_persistence;
mod epoch_handoff;
mod fixtures;
mod follower;
mod genesis_formation;
mod harness;
#[cfg(test)]
mod muxer_contract;
mod recovery;
/// Restart recovery: distinguishing a benign "execution head leads the marshal
/// finalized tip" restart (an unfinalized in-flight head) from genuine archive
/// corruption. See `unfinalized_head_lead_is_recoverable` + the recover match arm.
#[cfg(test)]
mod restart_recovery;
mod shutdown;
mod signer_replacement;

use fixtures::{
    recovery_block, recovery_finalization_fixture, run_test_dkg, run_test_dkg_complete,
    test_boundary_with_vrf_hash,
};

use harness::{start_recovery_marshal, start_recovery_marshal_with_reporter};
