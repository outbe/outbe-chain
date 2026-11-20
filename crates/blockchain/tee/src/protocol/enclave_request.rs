use alloy_primitives::B256;

use super::ParticipantAnnounce;

/// Direct resident infrastructure requests. Application protocols call the
/// resident context directly and do not travel through this enum.
#[derive(Clone, PartialEq, Eq)]
pub enum EnclaveRequest {
    /// Return the enclave's public keys.
    GetPublicKeys,
    /// Open a TEE DKG ceremony session inside the enclave.
    DkgOpen {
        ceremony_id: B256,
        round: u64,
        participants: Vec<ParticipantAnnounce>,
    },
    /// Seam A: deal + seal per-player shares.
    DkgStartDealer { ceremony_id: B256 },
    /// Seam B: open + verify an incoming sealed dealing inside the enclave.
    DkgPlayerIngest {
        ceremony_id: B256,
        dealer_bls: Vec<u8>,
        pub_msg: Vec<u8>,
        sealed_share: Vec<u8>,
    },
    /// Seam C: record a player's acknowledgement at this enclave's dealer.
    DkgDealerReceiveAck {
        ceremony_id: B256,
        player_bls: Vec<u8>,
        ack: Vec<u8>,
    },
    /// Seam D: finalize this enclave's dealing into a signed dealer log.
    DkgDealerFinalize { ceremony_id: B256 },
    /// Seam E: verify collected signed dealer logs and recover this enclave's local threshold share.
    DkgPlayerFinalize {
        ceremony_id: B256,
        signed_logs: Vec<Vec<u8>>,
    },
    /// Seam F (network encryption key): threshold-sign the fixed network-key message and seal partials to recipients.
    DkgNetworkKeyPartial { ceremony_id: B256 },
    /// Founding Seam F: finalize the initial group threshold signature and install the permanent keypair.
    DkgFinalizeNetworkKey {
        ceremony_id: B256,
        sealed_partials: Vec<Vec<u8>>,
        chain_id: B256,
        network_key_epoch: u64,
    },
    /// Produce this initialized enclave's exact ceremony-scoped DKG participant announcement.
    DkgParticipantAnnounceV1 {
        ceremony_id: B256,
        round: u64,
        participant_bls: Vec<Vec<u8>>,
    },
}

impl EnclaveRequest {
    /// Stable operation label for metrics and logs.
    pub const fn label(&self) -> &'static str {
        match self {
            Self::GetPublicKeys => "get_public_keys",
            Self::DkgOpen { .. } => "dkg_open",
            Self::DkgStartDealer { .. } => "dkg_start_dealer",
            Self::DkgPlayerIngest { .. } => "dkg_player_ingest",
            Self::DkgDealerReceiveAck { .. } => "dkg_dealer_receive_ack",
            Self::DkgDealerFinalize { .. } => "dkg_dealer_finalize",
            Self::DkgPlayerFinalize { .. } => "dkg_player_finalize",
            Self::DkgNetworkKeyPartial { .. } => "dkg_network_key_partial",
            Self::DkgFinalizeNetworkKey { .. } => "dkg_finalize_network_key",
            Self::DkgParticipantAnnounceV1 { .. } => "dkg_participant_announce_v1",
        }
    }
}

impl core::fmt::Debug for EnclaveRequest {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EnclaveRequest")
            .field("label", &self.label())
            .finish_non_exhaustive()
    }
}
