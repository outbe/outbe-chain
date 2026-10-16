//! Canonical publication commitments. Native objects and partial witnesses are verified separately.

pub mod binding;
pub mod merge;
pub mod policy;
pub mod review;
pub mod store;

use crate::manager::FinalizedPublicationReceipt;
use alloy_primitives::B256;
use commonware_cryptography::{Hasher as _, Sha256};
use outbe_primitives::radicle_publication::{PublicationAction, PublicationBody};
use radicle::{
    canonical::formatter::CanonicalFormatter, crypto::BlsEnvelope, git, storage::refs::Refs,
};
use serde::{Deserialize, Serialize};

pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;

#[cfg(test)]
pub(crate) mod tests;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("publication manifest exceeds one MiB")]
    Size,
    #[error("invalid or noncanonical publication manifest")]
    Manifest,
    #[error("publication manifest does not match the finalized receipt")]
    Receipt,
    #[error("publication owner snapshot does not match its previous checkpoint")]
    Snapshot,
    #[error("owner references conflict with the accepted publication")]
    RefConflict,
    #[error(transparent)]
    Git(#[from] git::raw::Error),
    #[error(transparent)]
    Storage(#[from] radicle::storage::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// Used only by sealed accepted proofs. Preserve unchanged refs and recover old-or-new deltas.
pub(crate) fn install_owner_refs(
    repo: &radicle::storage::git::Repository,
    body: &PublicationBody,
    manifest: &PublicationManifest,
) -> Result<(), Error> {
    use radicle::{
        crypto::{BlsPublicKey, RepositoryKey},
        storage::{ReadRepository as _, WriteRepository as _},
    };
    use std::collections::BTreeMap;
    if repo.id().into_sha1() != Some(body.repo_id.0) {
        return Err(Error::Receipt);
    }
    let owner = RepositoryKey::from(
        BlsPublicKey::from_bytes(body.owner_bls_public.0).map_err(|_| Error::Manifest)?,
    );
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
        let current = match repo.raw().find_reference(&name) {
            Ok(reference) => {
                let reference = if old == new {
                    reference.resolve()?
                } else {
                    reference
                };
                Some(git::Oid::from(
                    reference.target().ok_or(Error::RefConflict)?,
                ))
            }
            Err(error) if error.code() == git::raw::ErrorCode::NotFound => None,
            Err(error) => return Err(error.into()),
        };
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

/// The policy has verified every native owner entry and fenced the accepted sequence.
fn install_snapshot_refs(
    repo: &radicle::storage::git::Repository,
    snapshot: &CheckpointOwnerSnapshot,
) -> Result<(), Error> {
    use radicle::{
        crypto::{BlsPublicKey, RepositoryKey},
        storage::{ReadRepository as _, WriteRepository as _},
    };
    use std::collections::BTreeSet;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObjectKind {
    Blob,
    Commit,
    Tree,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectCommitment {
    pub kind: ObjectKind,
    pub oid: String,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefUpdate {
    pub name: String,
    pub old: String,
    pub new: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeSubject {
    pub ordinal: u8,
    pub envelope: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerRef {
    pub name: String,
    pub oid: String,
}

/// Construction and encoding enforce the canonical wire contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicationManifest {
    version: u8,
    objects: Vec<ObjectCommitment>,
    refs: Vec<RefUpdate>,
    subjects: Vec<NativeSubject>,
    owner_snapshot: Vec<OwnerRef>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ManifestWire {
    version: u8,
    objects: Vec<ObjectCommitment>,
    refs: Vec<RefUpdate>,
    subjects: Vec<NativeSubject>,
    owner_snapshot: Vec<OwnerRef>,
}

impl From<ManifestWire> for PublicationManifest {
    fn from(wire: ManifestWire) -> Self {
        Self {
            version: wire.version,
            objects: wire.objects,
            refs: wire.refs,
            subjects: wire.subjects,
            owner_snapshot: wire.owner_snapshot,
        }
    }
}

impl PublicationManifest {
    pub fn new(
        mut objects: Vec<ObjectCommitment>,
        mut refs: Vec<RefUpdate>,
        mut subjects: Vec<NativeSubject>,
        mut owner_snapshot: Vec<OwnerRef>,
    ) -> Result<Self, Error> {
        objects.sort_by(|a, b| (a.kind, &a.oid).cmp(&(b.kind, &b.oid)));
        refs.sort_by(|a, b| a.name.cmp(&b.name));
        subjects.sort_by_key(|s| s.ordinal);
        owner_snapshot.sort_by(|a, b| a.name.cmp(&b.name));
        let result = Self {
            version: 1,
            objects,
            refs,
            subjects,
            owner_snapshot,
        };
        result.to_bytes()?;
        Ok(result)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(Error::Size);
        }
        let manifest: Self = serde_json::from_slice::<ManifestWire>(bytes)?.into();
        if manifest.to_bytes()? != bytes {
            return Err(Error::Manifest);
        }
        Ok(manifest)
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        self.validate()?;
        let mut bytes = Vec::new();
        self.serialize(&mut serde_json::Serializer::with_formatter(
            &mut bytes,
            CanonicalFormatter::new(),
        ))?;
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(Error::Size);
        }
        // Canonical Unicode normalization must not silently change a Git ref name.
        if Self::from(serde_json::from_slice::<ManifestWire>(&bytes)?) != *self {
            return Err(Error::Manifest);
        }
        Ok(bytes)
    }

    pub fn hash(&self) -> Result<B256, Error> {
        Ok(Sha256::hash(&[&self.to_bytes()?]).0.into())
    }

    pub fn objects(&self) -> &[ObjectCommitment] {
        &self.objects
    }
    pub fn refs(&self) -> &[RefUpdate] {
        &self.refs
    }
    pub fn subjects(&self) -> &[NativeSubject] {
        &self.subjects
    }
    pub fn owner_snapshot(&self) -> &[OwnerRef] {
        &self.owner_snapshot
    }

    pub fn owner_refs(&self) -> Result<Refs, Error> {
        self.validate()?;
        self.owner_snapshot
            .iter()
            .map(|entry| {
                Ok((
                    qualified(&entry.name)?.to_ref_string(),
                    oid(&entry.oid, false)?,
                ))
            })
            .collect::<Result<Vec<_>, Error>>()
            .map(|entries| Refs::from(entries.into_iter()))
    }

    /// Checks the committed format/job fields; this is not native-object or witness verification.
    pub(crate) fn bind_receipt(&self, body: &PublicationBody) -> Result<(), Error> {
        if self.hash()? != body.package_manifest_hash {
            return Err(Error::Receipt);
        }
        let kinds: &[u8] = match body.action {
            PublicationAction::Bootstrap => &[1, 2, 3],
            PublicationAction::Review | PublicationAction::Merge => &[2, 3],
            PublicationAction::BindMaterial => {
                if !self.objects.is_empty() || !self.refs.is_empty() {
                    return Err(Error::Receipt);
                }
                &[]
            }
        };
        if self.subjects.len() != kinds.len() {
            return Err(Error::Receipt);
        }
        for (subject, kind) in self.subjects.iter().zip(kinds) {
            let envelope = BlsEnvelope::from_hex(&subject.envelope).map_err(|_| Error::Receipt)?;
            let encoded = envelope.to_bytes();
            let context = native_policy_context(body, subject.ordinal);
            if encoded[4] != *kind
                || encoded[5..25] != body.repo_id.0
                || encoded[25..57] != context
                || envelope.key().as_bytes().as_slice() != body.owner_bls_public.as_slice()
            {
                return Err(Error::Receipt);
            }
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), Error> {
        if self.version != 1
            || self.subjects.len() > 3
            || self
                .objects
                .windows(2)
                .any(|p| (p[0].kind, &p[0].oid) >= (p[1].kind, &p[1].oid))
            || self.refs.windows(2).any(|p| p[0].name >= p[1].name)
            || self
                .owner_snapshot
                .windows(2)
                .any(|p| p[0].name >= p[1].name)
        {
            return Err(Error::Manifest);
        }
        let mut object_ids = std::collections::BTreeSet::new();
        for object in &self.objects {
            oid(&object.oid, false)?;
            if !object_ids.insert(&object.oid) || !lower_hex(&object.sha256, 64) {
                return Err(Error::Manifest);
            }
        }
        for entry in &self.refs {
            qualified(&entry.name)?;
            oid(&entry.old, true)?;
            oid(&entry.new, true)?;
            if entry.old == entry.new {
                return Err(Error::Manifest);
            }
        }
        for (ordinal, subject) in self.subjects.iter().enumerate() {
            if subject.ordinal as usize != ordinal {
                return Err(Error::Manifest);
            }
            BlsEnvelope::from_hex(&subject.envelope).map_err(|_| Error::Manifest)?;
        }
        for entry in &self.owner_snapshot {
            let name = qualified(&entry.name)?;
            oid(&entry.oid, false)?;
            if name == *radicle::storage::refs::SIGREFS_BRANCH
                || name == *radicle::storage::refs::SIGREFS_PARENT
            {
                return Err(Error::Manifest);
            }
        }
        Ok(())
    }
}

/// Pinned owner refs commitments replayed from finalized receipt/manifest pairs.
/// Native objects and contribution witnesses require their separate verifier before publication.
#[derive(Clone)]
pub struct CheckpointOwnerSnapshot {
    checkpoint: FinalizedPublicationReceipt,
    refs: Refs,
    sigrefs: git::Oid,
}

impl CheckpointOwnerSnapshot {
    /// Only the protected local node transport may reconstruct its verified preparation refs.
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

    /// One immutable checkpoint at a time; BindMaterial preserves the last native sigrefs.
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
            || next.repo_id != old.repo_id
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

pub fn native_policy_context(body: &PublicationBody, ordinal: u8) -> [u8; 32] {
    Sha256::hash(&[
        b"OPJ1",
        &body.chain_id.to_be_bytes(),
        body.genesis_hash.as_slice(),
        body.repo_id.as_slice(),
        &body.proposal_id.to_be_bytes::<32>(),
        body.patch_id.as_slice(),
        body.revision_id.as_slice(),
        body.expected_target_head.as_slice(),
        &[body.action as u8],
        &[ordinal],
        &body.epoch.to_be_bytes(),
        &body.material_version.to_be_bytes(),
        body.material_hash.as_slice(),
        body.previous_receipt_hash.as_slice(),
    ])
    .0
}

fn lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn oid(value: &str, allow_zero: bool) -> Result<git::Oid, Error> {
    if !lower_hex(value, 40) {
        return Err(Error::Manifest);
    }
    let oid: git::Oid = value.parse().map_err(|_| Error::Manifest)?;
    if !allow_zero && oid.is_zero() {
        return Err(Error::Manifest);
    }
    Ok(oid)
}

fn qualified(name: &str) -> Result<git::fmt::Qualified<'_>, Error> {
    // Names are relative to the body owner's namespace, as in native signed refs.
    if name.starts_with("refs/namespaces/") {
        return Err(Error::Manifest);
    }
    git::fmt::Qualified::from_refstr(
        git::fmt::RefStr::try_from_str(name).map_err(|_| Error::Manifest)?,
    )
    .ok_or(Error::Manifest)
}
