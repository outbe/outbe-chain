use super::{oid, qualified, Error, ObjectKind, PublicationManifest};
use crate::manager::FinalizedPublicationReceipt;
use outbe_primitives::radicle_publication::PublicationAction;
use radicle::{git, storage::refs::Refs};

#[derive(Clone)]
pub struct CheckpointOwnerSnapshot {
    checkpoint: FinalizedPublicationReceipt,
    refs: Refs,
    sigrefs: git::Oid,
}

impl CheckpointOwnerSnapshot {
    pub(crate) fn from_local_refs(
        checkpoint: FinalizedPublicationReceipt,
        refs: Refs,
        sigrefs: git::Oid,
    ) -> Result<Self, Error> {
        if sigrefs.is_zero()
            || refs.get(&radicle::storage::refs::IDENTITY_ROOT).is_none()
            || refs.head(git::fmt::refname!("main")).is_none()
        {
            return Err(Error::Snapshot);
        }
        Ok(Self {
            checkpoint,
            refs,
            sigrefs,
        })
    }

    pub fn bootstrap(
        checkpoint: FinalizedPublicationReceipt,
        manifest: &PublicationManifest,
    ) -> Result<Self, Error> {
        let body = &checkpoint.receipt().body;
        if body.action != PublicationAction::Bootstrap
            || body.sequence != 1
            || !body.previous_receipt_hash.is_zero()
        {
            return Err(Error::Snapshot);
        }
        manifest.bind_receipt(body)?;
        let (refs, sigrefs) = apply_refs(&Refs::new(), git::Oid::ZERO_SHA1, manifest)?;
        if refs.get(&radicle::storage::refs::IDENTITY_ROOT).is_none()
            || refs.head(git::fmt::refname!("main")).is_none()
        {
            return Err(Error::Snapshot);
        }
        Ok(Self {
            checkpoint,
            refs,
            sigrefs,
        })
    }

    pub fn advance(
        &mut self,
        checkpoint: FinalizedPublicationReceipt,
        manifest: &PublicationManifest,
    ) -> Result<(), Error> {
        let old = &self.checkpoint.receipt().body;
        let next = &checkpoint.receipt().body;
        if old.sequence.checked_add(1) != Some(next.sequence)
            || next.previous_receipt_hash != self.checkpoint.receipt().receipt_hash
            || next.chain_id != old.chain_id
            || next.genesis_hash != old.genesis_hash
        {
            return Err(Error::Snapshot);
        }
        if next.repo_id != old.repo_id
            || next.owner_bls_public != old.owner_bls_public
            || next.descriptor_hash != old.descriptor_hash
            || next.action == PublicationAction::Bootstrap
        {
            return Err(Error::Snapshot);
        }
        manifest.bind_receipt(next)?;
        let (refs, sigrefs) = if next.action == PublicationAction::BindMaterial {
            if manifest.owner_refs()? != self.refs {
                return Err(Error::Snapshot);
            }
            (self.refs.clone(), self.sigrefs)
        } else {
            apply_refs(&self.refs, self.sigrefs, manifest)?
        };
        self.refs = refs;
        self.sigrefs = sigrefs;
        self.checkpoint = checkpoint;
        Ok(())
    }

    pub fn checkpoint(&self) -> &FinalizedPublicationReceipt {
        &self.checkpoint
    }
    pub fn owner_refs(&self) -> &Refs {
        &self.refs
    }
    pub fn sigrefs(&self) -> git::Oid {
        self.sigrefs
    }
}

fn apply_refs(
    previous: &Refs,
    previous_sigrefs: git::Oid,
    manifest: &PublicationManifest,
) -> Result<(Refs, git::Oid), Error> {
    let mut refs = previous.clone();
    let mut sigrefs = None;
    for update in &manifest.refs {
        let name = qualified(&update.name)?;
        let old = oid(&update.old, true)?;
        let new = oid(&update.new, true)?;
        if name == *radicle::storage::refs::SIGREFS_BRANCH {
            if old != previous_sigrefs
                || new.is_zero()
                || !manifest
                    .objects
                    .iter()
                    .any(|object| object.kind == ObjectKind::Commit && object.oid == update.new)
            {
                return Err(Error::Snapshot);
            }
            sigrefs = Some(new);
        } else {
            if old != refs.get(&name).unwrap_or(git::Oid::ZERO_SHA1) {
                return Err(Error::Snapshot);
            }
            refs.insert(name.to_ref_string(), new);
        }
    }
    let sigrefs = sigrefs.ok_or(Error::Snapshot)?;
    if refs != manifest.owner_refs()? {
        return Err(Error::Snapshot);
    }
    Ok((refs, sigrefs))
}
