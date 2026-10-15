// Copyright © 2022 The Radicle Link Contributors

use std::collections::BTreeMap;
use std::convert::TryFrom;
use std::path::PathBuf;
use std::sync::LazyLock;

use metadata::author::Author;
use metadata::commit::headers::Headers;
use metadata::commit::trailers::OwnedTrailer;
use nonempty::NonEmpty;
use oid::Oid;

use crate::change::store::Version;
use crate::signatures;
use crate::trailers::CommitTrailer;
use crate::{
    Embed, change,
    change::{Contents, Entry, Timestamp, store},
    signatures::{ExtendedSignature, Signatures},
    trailers,
};

use super::commit::Commit;

/// Name of the COB manifest file.
pub const MANIFEST_BLOB_NAME: &str = "manifest";
/// Path under which COB embeds are kept.
pub static EMBEDS_PATH: LazyLock<PathBuf> = LazyLock::new(|| PathBuf::from("embeds"));

pub mod error {
    use std::str::Utf8Error;
    use std::string::FromUtf8Error;

    use oid::Oid;
    use thiserror::Error;

    use crate::signatures::error::Signatures;

    #[derive(Debug, Error)]
    pub enum Create {
        #[error(transparent)]
        WriteCommit(#[from] super::super::commit::error::Write),
        #[error(transparent)]
        FromUtf8(#[from] FromUtf8Error),
        #[error(transparent)]
        Git(#[from] git2::Error),
        #[error(transparent)]
        Signer(#[from] Box<dyn std::error::Error + Send + Sync + 'static>),
        #[error(transparent)]
        Signatures(#[from] Signatures),
        #[error(transparent)]
        Utf8(#[from] Utf8Error),
    }

    #[derive(Debug, Error)]
    pub enum Load {
        #[error("BLS entry requires an explicit repository and trusted signing context")]
        NativeContextRequired,
        #[error(transparent)]
        NativeMetadata(#[from] metadata::commit::bls::Error),
        #[error(transparent)]
        NativeSignature(#[from] crypto::repository::Error),
        #[error(transparent)]
        WriteCommit(#[from] super::super::commit::error::Write),
        #[error(transparent)]
        Read(#[from] super::super::commit::error::Read),
        #[error(transparent)]
        Signatures(#[from] Signatures),
        #[error(transparent)]
        Git(#[from] git2::Error),
        #[error("a 'manifest' file was expected be found in '{0}'")]
        NoManifest(Oid),
        #[error("the 'manifest' found at '{0}' was not a blob")]
        ManifestIsNotBlob(Oid),
        #[error("the 'manifest' found at '{id}' was invalid: {err}")]
        InvalidManifest {
            id: Oid,
            #[source]
            err: serde_json::Error,
        },
        #[error("a 'change' file was expected be found in '{0}'")]
        NoChange(Oid),
        #[error("the 'change' found at '{0}' was not a blob")]
        ChangeNotBlob(Oid),
        #[error("the 'change' found at '{0}' was not signed")]
        ChangeNotSigned(Oid),
        #[error("the 'change' found at '{0}' has more than one signature")]
        TooManySignatures(Oid),
        #[error("the 'change' found at '{0}' has more than one resource trailer")]
        TooManyResources(Oid),
        #[error(transparent)]
        ResourceTrailer(#[from] super::trailers::error::InvalidResourceTrailer),
        #[error("non utf-8 characters in commit message")]
        Utf8(#[from] FromUtf8Error),
    }
}

impl change::Storage for git2::Repository {
    type StoreError = error::Create;
    type LoadError = error::Load;

    type ObjectId = Oid;
    type Parent = Oid;

    type PublicKey = crypto::RepositoryKey;
    type Signature = change::EntrySignature;

    fn store(
        &self,
        resource: Option<Self::Parent>,
        mut related: Vec<Self::Parent>,
        signer: &impl crypto::Signer,
        spec: store::Template<Self::ObjectId>,
    ) -> Result<Entry, Self::StoreError> {
        let change::Template {
            type_name,
            tips,
            message,
            embeds,
            contents,
        } = spec;
        let manifest = store::Manifest::new(type_name, Version::default());
        let revision = write_manifest(self, &manifest, embeds, &contents)?;
        let tree = self.find_tree(revision)?;
        let signature = ExtendedSignature::try_sign(signer, revision.as_bytes())
            .map_err(|source| error::Create::Signer(Box::new(source)))?;

        // Make sure there are no duplicates in the related list.
        related.sort();
        related.dedup();

        let (id, timestamp) = write_commit(
            self,
            resource.map(|o| o.into()),
            // Commit to tips, extra parents and resource.
            tips.iter()
                .cloned()
                .chain(related.clone())
                .chain(resource)
                .map(git2::Oid::from),
            message,
            signature.clone(),
            related
                .iter()
                .map(|p| trailers::CommitTrailer::Related(*p).into()),
            tree,
        )?;

        Ok(Entry {
            id,
            revision: revision.into(),
            signature: change::EntrySignature::legacy(signature),
            resource,
            parents: tips,
            related,
            manifest,
            contents,
            timestamp,
        })
    }

    fn parents_of(&self, id: &Oid) -> Result<Vec<Oid>, Self::LoadError> {
        Ok(self
            .find_commit(id.into())?
            .parent_ids()
            .map(Oid::from)
            .collect::<Vec<_>>())
    }

    fn manifest_of(&self, id: &Oid) -> Result<crate::Manifest, Self::LoadError> {
        let commit = self.find_commit(id.into())?;
        let tree = commit.tree()?;
        load_manifest(self, &tree)
    }

    fn load(&self, id: Self::ObjectId) -> Result<Entry, Self::LoadError> {
        let commit = super::commit::Commit::read(self, id.into())?;
        if commit.headers().any(|(name, _)| name == "radicle-bls-sig") {
            return Err(error::Load::NativeContextRequired);
        }
        let mut signatures = Signatures::try_from(&*commit)?
            .into_iter()
            .collect::<Vec<_>>();
        let Some((key, sig)) = signatures.pop() else {
            return Err(error::Load::ChangeNotSigned(id));
        };
        if !signatures.is_empty() {
            return Err(error::Load::TooManySignatures(id));
        }
        load_entry(
            self,
            id,
            &commit,
            change::EntrySignature::legacy(ExtendedSignature::new(key, sig)),
        )
    }
}

/// Load BLS using an independently supplied repository and trusted policy context.
/// Raw `git2::Repository::load` remains Ed25519-only because it has neither.
pub fn load_native(
    repo: &git2::Repository,
    id: Oid,
    repository: [u8; 20],
    context: [u8; 32],
) -> Result<Entry, error::Load> {
    let odb = repo.odb()?;
    let raw = odb.read(id.into())?;
    if raw.kind() != git2::ObjectType::Commit {
        return Err(git2::Error::from_str("COB entry is not a commit").into());
    }
    let signed = metadata::commit::bls::SignedCommit::parse(raw.data())?;
    let signature = change::EntrySignature::bls(id, repository, context, &signed)?;
    let commit = Commit::read(repo, id.into())?;
    load_entry(repo, id, &commit, signature)
}

/// Attach an external signature to exact prepared metadata. This writes no refs.
/// The caller supplies the actual repository and trusted context, never envelope echoes.
pub fn attach_native(
    repo: &git2::Repository,
    prepared: metadata::commit::bls::UnsignedCommit,
    repository: [u8; 20],
    context: [u8; 32],
    envelope: &crypto::BlsEnvelope,
) -> Result<Entry, error::Load> {
    crypto::RepositorySignature::from(envelope.clone()).verify_cob(
        envelope.key().into(),
        repository,
        context,
        prepared.tree(),
        prepared.as_bytes(),
    )?;
    let signed = prepared.attach(&envelope.to_hex())?;
    let buffer = signed.to_bytes();
    let commit = Commit::try_from(buffer.as_slice()).map_err(super::commit::error::Read::from)?;
    let odb = repo.odb()?;
    commit.verify_for_write(&odb)?;
    let id = odb.write(git2::ObjectType::Commit, &buffer)?;
    load_native(repo, id.into(), repository, context)
}

fn load_entry(
    repo: &git2::Repository,
    id: Oid,
    commit: &Commit,
    signature: ExtendedSignature<crypto::RepositoryKey, change::EntrySignature>,
) -> Result<Entry, error::Load> {
    let timestamp = commit.committer().time.seconds() as u64;
    let trailers = parse_trailers(commit.trailers())?;
    let (resources, related): (Vec<_>, Vec<_>) = trailers.iter().partition(|t| match t {
        CommitTrailer::Resource(_) => true,
        CommitTrailer::Related(_) => false,
    });
    let mut resources = resources.into_iter().map(|r| r.oid()).collect::<Vec<_>>();
    let related = related.into_iter().map(|r| r.oid()).collect::<Vec<_>>();
    let parents = commit
        .parents()
        .map(Oid::from)
        .filter(|p| !resources.contains(p) && !related.contains(p))
        .collect();
    if resources.len() > 1 {
        return Err(error::Load::TooManyResources(id));
    };

    let tree = repo.find_tree(*commit.tree())?;
    let manifest = load_manifest(repo, &tree)?;
    let contents = load_contents(repo, &tree)?;

    Ok(Entry {
        id,
        revision: tree.id().into(),
        signature,
        resource: resources.pop(),
        related,
        parents,
        manifest,
        contents,
        timestamp,
    })
}

fn parse_trailers<'a>(
    trailers: impl Iterator<Item = &'a OwnedTrailer>,
) -> Result<Vec<trailers::CommitTrailer>, error::Load> {
    let mut parsed = Vec::new();
    for trailer in trailers {
        match trailers::CommitTrailer::try_from(trailer) {
            Err(trailers::error::InvalidResourceTrailer::WrongToken) => {
                continue;
            }
            Err(err) => return Err(err.into()),
            Ok(t) => parsed.push(t),
        }
    }
    Ok(parsed)
}

fn load_manifest(
    repo: &git2::Repository,
    tree: &git2::Tree,
) -> Result<store::Manifest, error::Load> {
    let manifest_tree_entry = tree
        .get_name(MANIFEST_BLOB_NAME)
        .ok_or_else(|| error::Load::NoManifest(tree.id().into()))?;
    let manifest_object = manifest_tree_entry.to_object(repo)?;
    let manifest_blob = manifest_object
        .as_blob()
        .ok_or_else(|| error::Load::ManifestIsNotBlob(tree.id().into()))?;

    serde_json::from_slice(manifest_blob.content()).map_err(|err| error::Load::InvalidManifest {
        id: tree.id().into(),
        err,
    })
}

fn load_contents(repo: &git2::Repository, tree: &git2::Tree) -> Result<Contents, error::Load> {
    let ops = tree
        .iter()
        .filter_map(|entry| {
            entry.kind().and_then(|kind| match kind {
                git2::ObjectType::Blob => {
                    let name = entry.name().ok()?.parse::<i8>().ok()?;
                    let blob = entry
                        .to_object(repo)
                        .and_then(|object| object.peel_to_blob())
                        .map(|blob| blob.content().to_owned())
                        .map(|b| (name, b));

                    Some(blob)
                }
                _ => None,
            })
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;

    NonEmpty::collect(ops.into_values()).ok_or_else(|| error::Load::NoChange(tree.id().into()))
}

fn write_commit(
    repo: &git2::Repository,
    resource: Option<git2::Oid>,
    parents: impl IntoIterator<Item = git2::Oid>,
    message: String,
    signature: ExtendedSignature,
    trailers: impl IntoIterator<Item = OwnedTrailer>,
    tree: git2::Tree,
) -> Result<(Oid, Timestamp), error::Create> {
    let trailers: Vec<OwnedTrailer> = trailers
        .into_iter()
        .chain(resource.map(|r| trailers::CommitTrailer::Resource(r.into()).into()))
        .collect();
    let author = repo.signature()?;
    #[allow(unused_variables)]
    let timestamp = author.when().seconds();

    let mut headers = Headers::new();
    headers.push(
        "gpgsig",
        signature
            .to_pem()
            .map_err(signatures::error::Signatures::from)?
            .as_str(),
    );

    let author = Author {
        name: String::from_utf8(author.name_bytes().to_vec())?,
        email: String::from_utf8(author.email_bytes().to_vec())?,
        time: {
            let when = author.when();
            metadata::author::Time::new(when.seconds(), when.offset_minutes())
        },
    };

    #[cfg(feature = "stable-commit-ids")]
    // Ensures the commit id doesn't change on every run.
    let (author, timestamp) = {
        let stable = crate::stable::read_timestamp();
        (
            Author {
                time: metadata::author::Time::new(stable, 0),
                ..author
            },
            stable,
        )
    };
    let (author, timestamp) = if let Ok(s) = std::env::var(super::GIT_COMMITTER_DATE) {
        let Ok(timestamp) = s.trim().parse::<i64>() else {
            panic!(
                "Invalid timestamp value {s:?} for `{}`",
                super::GIT_COMMITTER_DATE
            );
        };
        let author = Author {
            time: metadata::author::Time::new(timestamp, 0),
            ..author
        };
        (author, timestamp)
    } else {
        (author, timestamp)
    };

    let oid = Commit::new(
        tree.id(),
        parents,
        author.clone(),
        author,
        headers,
        message,
        trailers,
    )
    .write(repo)?;

    Ok((Oid::from(oid), timestamp as u64))
}

fn write_manifest(
    repo: &git2::Repository,
    manifest: &store::Manifest,
    embeds: Vec<Embed<Oid>>,
    contents: &NonEmpty<Vec<u8>>,
) -> Result<git2::Oid, git2::Error> {
    let mut root = repo.treebuilder(None)?;

    // Insert manifest file into tree.
    {
        // SAFETY: we're serializing to an in memory buffer so the only source of
        // errors here is a programming error, which we can't recover from.
        #[allow(clippy::unwrap_used)]
        let manifest = serde_json::to_vec(manifest).unwrap();
        let manifest_oid = repo.blob(&manifest)?;

        root.insert(
            MANIFEST_BLOB_NAME,
            manifest_oid,
            git2::FileMode::Blob.into(),
        )?;
    }

    // Insert each COB entry.
    for (ix, op) in contents.iter().enumerate() {
        let oid = repo.blob(op.as_ref())?;
        root.insert(ix.to_string(), oid, git2::FileMode::Blob.into())?;
    }

    // Insert each embed in a tree at `/embeds`.
    if !embeds.is_empty() {
        let mut embeds_tree = repo.treebuilder(None)?;

        for embed in embeds {
            let oid = embed.content;
            let path = PathBuf::from(embed.name);

            embeds_tree.insert(path, oid.into(), git2::FileMode::Blob.into())?;
        }
        let oid = embeds_tree.write()?;

        root.insert(&*EMBEDS_PATH, oid, git2::FileMode::Tree.into())?;
    }
    let oid = root.write()?;

    Ok(oid)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod native_test {
    use super::*;
    use crate::change::Storage as _;
    use crypto::Signer as _;
    use metadata::commit::bls::UnsignedCommit;

    const OP: &[u8] = br#"{"revision":"3333333333333333333333333333333333333333","type":"review","verdict":"accept"}"#;

    fn bytes(input: &str) -> Vec<u8> {
        input
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }

    #[test]
    fn external_threshold_signature_attaches_loads_and_binds_actual_git_entry() {
        let temp = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init_bare(temp.path()).unwrap();
        let vectors: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../radicle-crypto/src/repository/vectors.json"
        ))
        .unwrap();
        let row = &vectors["vectors"][1];
        let repository = bytes(vectors["repo"].as_str().unwrap()).try_into().unwrap();
        let context = bytes(row["context"].as_str().unwrap()).try_into().unwrap();
        let unsigned = bytes(vectors["unsigned_commit"].as_str().unwrap());
        let prepared = UnsignedCommit::parse(&unsigned).unwrap();
        let envelope = crypto::BlsEnvelope::from_hex(row["envelope"].as_str().unwrap()).unwrap();
        let tree = write_manifest(
            &repo,
            &store::Manifest::new("xyz.radicle.patch".parse().unwrap(), Version::default()),
            vec![],
            &NonEmpty::new(OP.to_vec()),
        )
        .unwrap();
        assert_eq!(tree.as_bytes(), prepared.tree());
        let entry = attach_native(&repo, prepared.clone(), repository, context, &envelope).unwrap();
        assert!(entry.valid_signatures());
        assert_eq!(*entry.author(), crypto::RepositoryKey::from(envelope.key()));
        assert_eq!(entry.revision, Oid::from(tree));
        assert_ne!(entry.id, entry.revision);
        assert_eq!(
            entry.id.to_string(),
            include_str!("../../../../radicle-git-metadata/src/commit/bls/expected-oid.txt")
        );
        assert_eq!(entry.contents.head, OP);
        assert_eq!(entry.timestamp, 1700000000);
        let raw = repo
            .odb()
            .unwrap()
            .read(entry.id.into())
            .unwrap()
            .data()
            .to_vec();
        let expected = prepared
            .clone()
            .attach(&envelope.to_hex())
            .unwrap()
            .to_bytes();
        assert_eq!(raw, expected);
        assert_eq!(
            entry.id,
            git2::Oid::hash_object(git2::ObjectType::Commit, &raw).unwrap()
        );
        assert!(repo.references().unwrap().next().is_none());
        assert!(matches!(
            repo.load(entry.id),
            Err(error::Load::NativeContextRequired)
        ));
        assert!(load_native(&repo, entry.id, [0; 20], context).is_err());
        assert!(load_native(&repo, entry.id, repository, [0; 32]).is_err());
        assert!(attach_native(&repo, prepared.clone(), [0; 20], context, &envelope).is_err());
        for altered in [
            String::from_utf8(unsigned.clone())
                .unwrap()
                .replace("1700000000", "1700000001"),
            String::from_utf8(unsigned.clone()).unwrap().replace(
                "Accept patch\n",
                "Accept patch\n\nRad-Resource: 1111111111111111111111111111111111111111\n",
            ),
        ] {
            let p = UnsignedCommit::parse(altered.as_bytes()).unwrap();
            assert!(attach_native(&repo, p, repository, context, &envelope).is_err());
        }
        for altered in [
            String::from_utf8(raw.clone())
                .unwrap()
                .replace("Accept patch\n", "Reject patch\n"),
            String::from_utf8(raw.clone())
                .unwrap()
                .replace("committer Outbe", "committer Other"),
            String::from_utf8(raw.clone())
                .unwrap()
                .replace("radicle-bls-sig ", "radicle-bls-sig 00"),
        ] {
            let id = repo
                .odb()
                .unwrap()
                .write(git2::ObjectType::Commit, altered.as_bytes())
                .unwrap();
            assert!(load_native(&repo, id.into(), repository, context).is_err());
        }
        let mut changed = entry.clone();
        changed.id = tree.into();
        assert!(!changed.valid_signatures());
        let mut changed = entry.clone();
        changed.revision = entry.id;
        assert!(!changed.valid_signatures());
        let mut changed = entry.clone();
        changed.signature = ExtendedSignature::new(
            crypto::RepositoryKey::from(*crypto::SigningKey::mock(42).public_key()),
            changed.signature.sig().clone(),
        );
        assert!(!changed.valid_signatures());
        drop(repo);
        let reopened = git2::Repository::open_bare(temp.path()).unwrap();
        assert_eq!(
            load_native(&reopened, entry.id, repository, context).unwrap(),
            entry
        );
    }

    #[test]
    fn legacy_ed_entries_remain_valid_and_mixed_signature_headers_are_rejected() {
        let storage = crate::test::Storage::new();
        let repo = storage.as_raw();
        let signer = crypto::SigningKey::mock(42);
        let entry = repo
            .store(
                None,
                vec![],
                &signer,
                store::Template {
                    type_name: "xyz.radicle.patch".parse().unwrap(),
                    tips: vec![],
                    message: "Legacy entry".into(),
                    embeds: vec![],
                    contents: NonEmpty::new(OP.to_vec()),
                },
            )
            .unwrap();
        assert!(entry.valid_signatures());
        assert_eq!(repo.load(entry.id).unwrap(), entry);
        let raw = repo
            .odb()
            .unwrap()
            .read(entry.id.into())
            .unwrap()
            .data()
            .to_vec();
        let raw = String::from_utf8(raw).unwrap();
        let mixed = raw.replacen("\n\n", "\nradicle-bls-sig bogus\n\n", 1);
        let id = repo
            .odb()
            .unwrap()
            .write(git2::ObjectType::Commit, mixed.as_bytes())
            .unwrap();
        assert!(matches!(
            repo.load(id.into()),
            Err(error::Load::NativeContextRequired)
        ));
        assert!(load_native(repo, id.into(), [0; 20], [0; 32]).is_err());
    }
}
