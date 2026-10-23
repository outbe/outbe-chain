use super::{CheckpointOwnerSnapshot, Error, PublicationManifest};
use outbe_primitives::radicle_publication::PublicationBody;
use radicle::{
    crypto::{BlsPublicKey, RepositoryKey},
    git,
    storage::{ReadRepository as _, WriteRepository as _},
};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn install_owner_refs(
    repo: &radicle::storage::git::Repository,
    body: &PublicationBody,
    manifest: &PublicationManifest,
) -> Result<(), Error> {
    if repo.id().into_sha1() != Some(body.repo_id.0) {
        return Err(Error::Receipt);
    }
    let owner = RepositoryKey::from(
        BlsPublicKey::from_bytes(body.owner_bls_public.0).map_err(|_| Error::Manifest)?,
    );
    let updates = owner_updates(manifest)?;
    let mut transaction = repo.raw().transaction()?;
    let qualified: Vec<_> = updates
        .iter()
        .map(|(name, (old, new))| {
            let name = git::fmt::Qualified::from_refstr(name)
                .ok_or(Error::Manifest)?
                .with_namespace((&owner).into())
                .to_string();
            Ok((name, *old, *new))
        })
        .collect::<Result<_, Error>>()?;
    for (name, _, _) in &qualified {
        transaction.lock_ref(name)?;
    }
    if repo
        .references_of(&owner)?
        .iter()
        .any(|(name, _)| !updates.contains_key(name))
    {
        return Err(Error::RefConflict);
    }
    for (name, old, new) in qualified {
        let current = current_target(repo, &name, old == new)?;
        if current == Some(new) {
            continue;
        }
        if current != (!old.is_zero()).then_some(old) {
            return Err(Error::RefConflict);
        }
        transaction.set_target(&name, new.into(), None, "Outbe accepted publication")?;
    }
    transaction.commit()?;
    Ok(())
}

fn owner_updates(
    manifest: &PublicationManifest,
) -> Result<BTreeMap<git::fmt::RefString, (git::Oid, git::Oid)>, Error> {
    let mut updates = BTreeMap::new();
    for row in manifest.owner_snapshot() {
        let name = git::fmt::RefString::try_from(row.name.clone()).map_err(|_| Error::Manifest)?;
        let oid: git::Oid = row.oid.parse().map_err(|_| Error::Manifest)?;
        updates.insert(name, (oid, oid));
    }
    for update in manifest.refs() {
        let name =
            git::fmt::RefString::try_from(update.name.clone()).map_err(|_| Error::Manifest)?;
        let old: git::Oid = update.old.parse().map_err(|_| Error::Manifest)?;
        let new: git::Oid = update.new.parse().map_err(|_| Error::Manifest)?;
        if new.is_zero()
            || (name.as_str() != radicle::storage::refs::SIGREFS_BRANCH.as_str()
                && updates.get(&name).map(|(_, oid)| *oid) != Some(new))
        {
            return Err(Error::Manifest);
        }
        updates.insert(name, (old, new));
    }
    Ok(updates)
}

fn current_target(
    repo: &radicle::storage::git::Repository,
    name: &str,
    resolve: bool,
) -> Result<Option<git::Oid>, Error> {
    match repo.raw().find_reference(name) {
        Ok(reference) => {
            let reference = if resolve {
                reference.resolve()?
            } else {
                reference
            };
            Ok(Some(git::Oid::from(
                reference.target().ok_or(Error::RefConflict)?,
            )))
        }
        Err(error) if error.code() == git::raw::ErrorCode::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn install_snapshot_refs(
    repo: &radicle::storage::git::Repository,
    snapshot: &CheckpointOwnerSnapshot,
) -> Result<(), Error> {
    let body = &snapshot.checkpoint().receipt().body;
    if repo.id().into_sha1() != Some(body.repo_id.0) {
        return Err(Error::Receipt);
    }
    let owner = RepositoryKey::from(
        BlsPublicKey::from_bytes(body.owner_bls_public.0).map_err(|_| Error::Manifest)?,
    );
    let actual = repo.references_of(&owner)?;
    let mut desired = snapshot.owner_refs().clone();
    desired.insert(
        radicle::storage::refs::SIGREFS_BRANCH.to_ref_string(),
        snapshot.sigrefs(),
    );
    let names: BTreeSet<_> = actual
        .iter()
        .chain(desired.iter())
        .map(|(name, _)| name.clone())
        .collect();
    let qualified = |name: &git::fmt::RefString| -> Result<String, Error> {
        Ok(git::fmt::Qualified::from_refstr(name)
            .ok_or(Error::Manifest)?
            .with_namespace((&owner).into())
            .to_string())
    };
    let mut transaction = repo.raw().transaction()?;
    for name in &names {
        transaction.lock_ref(&qualified(name)?)?;
    }
    if repo.references_of(&owner)? != actual {
        return Err(Error::RefConflict);
    }
    for name in names {
        let reference = git::fmt::Qualified::from_refstr(&name).ok_or(Error::Manifest)?;
        let target = desired.get(&reference);
        if target == actual.get(&reference) {
            continue;
        }
        let name = qualified(&name)?;
        match target {
            Some(target) => {
                transaction.set_target(&name, target.into(), None, "Outbe accepted snapshot")?
            }
            None => transaction.remove(&name)?,
        }
    }
    transaction.commit()?;
    Ok(())
}
