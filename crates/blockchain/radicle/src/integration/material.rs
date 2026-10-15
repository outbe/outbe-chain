//! One current public DKG output, installed only by the engine's authenticated lifecycle.

use super::signing::{Error, RepositorySigningMaterial};
use crate::manager::FinalizedRepositoryAuthority;
use commonware_cryptography::bls12381::{
    self, dkg::feldman_desmedt::Output, primitives::variant::MinSig,
};
use radicle::crypto::BlsPublicKey;
use std::sync::Arc;
use tokio::sync::watch;

struct CurrentOutput {
    epoch: u64,
    version: u64,
    output: Output<MinSig, bls12381::PublicKey>,
}

/// Engine-owned publisher. Dropping its unique owner withdraws the public material.
pub struct RepositoryMaterialPublisher(watch::Sender<Option<Arc<CurrentOutput>>>);

pub(crate) struct RepositoryMaterialHandle(watch::Receiver<Option<Arc<CurrentOutput>>>);

pub(crate) fn channel() -> (RepositoryMaterialPublisher, RepositoryMaterialHandle) {
    let (sender, receiver) = watch::channel(None);
    (
        RepositoryMaterialPublisher(sender),
        RepositoryMaterialHandle(receiver),
    )
}

impl RepositoryMaterialPublisher {
    /// `output` comes from active authenticated engine state, never a peer request.
    pub fn publish(
        &self,
        epoch: u64,
        version: u64,
        output: Option<&Output<MinSig, bls12381::PublicKey>>,
    ) {
        self.0.send_replace(output.map(|output| {
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
        self.0.send_replace(None);
    }
}

impl RepositoryMaterialHandle {
    pub(crate) fn current(
        &self,
        authority: FinalizedRepositoryAuthority,
        owner: BlsPublicKey,
    ) -> Result<RepositorySigningMaterial, Error> {
        let active = self.0.borrow().clone().ok_or(Error::Unavailable)?;
        if active.epoch != authority.epoch()
            || active.version != authority.committee().vrf_material_version
        {
            return Err(Error::Material);
        }
        RepositorySigningMaterial::new(authority, active.output.clone(), owner)
    }
}
