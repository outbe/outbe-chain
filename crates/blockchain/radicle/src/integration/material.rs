//! One current public DKG output, installed only by the engine's authenticated lifecycle.

use super::signing::{Error, RepositorySigningMaterial};
use crate::manager::FinalizedRepositoryAuthority;
use commonware_cryptography::bls12381::{
    self, dkg::feldman_desmedt::Output, primitives::variant::MinSig,
};
use outbe_consensus::hybrid::VrfMaterialProvider;
use radicle::crypto::BlsPublicKey;
use std::sync::Arc;
use tokio::sync::watch;

struct CurrentOutput {
    epoch: u64,
    version: u64,
    output: Output<MinSig, bls12381::PublicKey>,
}

/// Engine-owned publisher. Dropping its unique owner withdraws the public material.
pub struct RepositoryMaterialPublisher {
    output: watch::Sender<Option<Arc<CurrentOutput>>>,
    local_signer: watch::Sender<Option<VrfMaterialProvider<MinSig>>>,
}

pub(crate) struct RepositoryMaterialHandle {
    output: watch::Receiver<Option<Arc<CurrentOutput>>>,
    local_signer: watch::Receiver<Option<VrfMaterialProvider<MinSig>>>,
}

pub(crate) fn channel() -> (RepositoryMaterialPublisher, RepositoryMaterialHandle) {
    let (sender, receiver) = watch::channel(None);
    let (signer, signer_receiver) = watch::channel(None);
    (
        RepositoryMaterialPublisher {
            output: sender,
            local_signer: signer,
        },
        RepositoryMaterialHandle {
            output: receiver,
            local_signer: signer_receiver,
        },
    )
}

impl RepositoryMaterialPublisher {
    /// Same-process capability only; no share is extracted or serialized.
    pub fn install_local_signer(&self, provider: VrfMaterialProvider<MinSig>) {
        self.local_signer.send_replace(Some(provider));
    }
    /// `output` comes from active authenticated engine state, never a peer request.
    pub fn publish(
        &self,
        epoch: u64,
        version: u64,
        output: Option<&Output<MinSig, bls12381::PublicKey>>,
    ) {
        self.output.send_replace(output.map(|output| {
            Arc::new(CurrentOutput {
                epoch,
                version,
                output: output.clone(),
            })
        }));
    }
}

impl Drop for RepositoryMaterialPublisher {
    fn drop(&mut self) {
        self.output.send_replace(None);
        self.local_signer.send_replace(None);
    }
}

impl RepositoryMaterialHandle {
    pub(crate) fn local_signer(&self) -> Result<VrfMaterialProvider<MinSig>, Error> {
        self.local_signer.borrow().clone().ok_or(Error::Unavailable)
    }
    pub(crate) fn current(
        &self,
        authority: FinalizedRepositoryAuthority,
        owner: BlsPublicKey,
    ) -> Result<RepositorySigningMaterial, Error> {
        let active = self.output.borrow().clone().ok_or(Error::Unavailable)?;
        if active.epoch != authority.epoch()
            || active.version != authority.committee().vrf_material_version
        {
            return Err(Error::Material);
        }
        RepositorySigningMaterial::new(authority, active.output.clone(), owner)
    }
}
