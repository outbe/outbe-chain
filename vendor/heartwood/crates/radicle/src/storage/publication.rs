//! Trusted local authorization for managed repository entries. No network policy is inferred.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crypto::{RepositoryKey, SubjectKind};
use serde::{Deserialize, Serialize};

use crate::git::Oid;
use crate::identity::RepoId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryRequest {
    pub repository: RepoId,
    pub authority: RepositoryKey,
    /// Actual containing COB or signed-refs commit, also for identity action signatures.
    pub entry: Oid,
    pub kind: SubjectKind,
}

/// A trusted provider binds the actual request to an immutable accepted receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryAuthorization {
    request: EntryRequest,
    context: [u8; 32],
    receipt: Option<[u8; 32]>,
}

impl EntryAuthorization {
    pub fn accepted(request: EntryRequest, context: [u8; 32], receipt: [u8; 32]) -> Self {
        Self {
            request,
            context,
            receipt: Some(receipt),
        }
    }

    pub fn context(&self) -> [u8; 32] {
        self.context
    }
    pub fn receipt(&self) -> Option<[u8; 32]> {
        self.receipt
    }

    pub(crate) fn native(request: EntryRequest) -> Self {
        Self {
            request,
            context: [0; 32],
            receipt: None,
        }
    }
}

pub trait PublicationPolicy: std::fmt::Debug + Send + Sync {
    fn id(&self) -> &str;
    fn authorize_entry(&self, request: &EntryRequest) -> Result<EntryAuthorization, Error>;
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("managed repository publication policy is unavailable")]
    Unavailable,
    #[error("repository entry has no accepted publication receipt")]
    Denied,
    #[error("publication authorization does not bind the actual request or managed context")]
    InvalidAuthorization,
    #[error("invalid or conflicting trusted managed-repository configuration")]
    Configuration,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Managed {
    version: u8,
    repository: RepoId,
    owner: RepositoryKey,
    policy: String,
}

/// Shared by storage clones; direct opens still read persistent managed requirements.
#[derive(Debug)]
pub(crate) struct Registry {
    root: PathBuf,
    providers: RwLock<BTreeMap<RepoId, Arc<dyn PublicationPolicy>>>,
}

impl Registry {
    pub(crate) fn new(root: &Path) -> Self {
        Self {
            root: root.join(".publication"),
            providers: RwLock::new(BTreeMap::new()),
        }
    }

    fn path(&self, repository: RepoId) -> PathBuf {
        self.root.join(format!("{}.json", repository.canonical()))
    }

    fn record(&self, repository: RepoId) -> Result<Option<Managed>, Error> {
        let path = self.path(repository);
        let file = match fs::File::open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut data = Vec::new();
        file.take(4097).read_to_end(&mut data)?;
        if data.len() > 4096 {
            return Err(Error::Configuration);
        }
        let record: Managed = serde_json::from_slice(&data)?;
        if record.version != 1
            || record.repository != repository
            || record.owner.as_ed25519().is_some()
        {
            return Err(Error::Configuration);
        }
        Ok(Some(record))
    }

    pub(crate) fn manage(
        &self,
        repository: RepoId,
        owner: RepositoryKey,
        provider: Arc<dyn PublicationPolicy>,
    ) -> Result<(), Error> {
        let policy = provider.id();
        if owner.as_ed25519().is_some()
            || policy.is_empty()
            || policy.len() > 128
            || !policy.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(Error::Configuration);
        }
        let wanted = Managed {
            version: 1,
            repository,
            owner,
            policy: policy.to_owned(),
        };
        fs::create_dir_all(&self.root)?;
        if let Some(existing) = self.record(repository)? {
            if existing != wanted {
                return Err(Error::Configuration);
            }
        } else {
            let temporary = self.root.join(format!(
                ".{}.{}.tmp",
                repository.canonical(),
                fastrand::u128(..)
            ));
            let result = (|| {
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&temporary)?;
                file.write_all(&serde_json::to_vec(&wanted)?)?;
                file.sync_all()?;
                // Create-only publication prevents concurrent registration from changing the owner.
                match fs::hard_link(&temporary, self.path(repository)) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                        if self.record(repository)?.as_ref() != Some(&wanted) {
                            return Err(Error::Configuration);
                        }
                    }
                    Err(e) => return Err(e.into()),
                }
                fs::File::open(&self.root)?.sync_all()?;
                Ok::<_, Error>(())
            })();
            let _ = fs::remove_file(&temporary);
            result?;
        }
        self.providers
            .write()
            .map_err(|_| Error::Unavailable)?
            .insert(repository, provider);
        Ok(())
    }

    pub(crate) fn authorize(&self, request: EntryRequest) -> Result<EntryAuthorization, Error> {
        if request.authority.as_ed25519().is_some() {
            return Ok(EntryAuthorization::native(request));
        }
        let provider = self
            .providers
            .read()
            .map_err(|_| Error::Unavailable)?
            .get(&request.repository)
            .cloned();
        let Some(record) = self.record(request.repository)? else {
            return if provider.is_some() {
                Err(Error::Configuration)
            } else {
                Ok(EntryAuthorization::native(request))
            };
        };
        if record.owner != request.authority {
            return Ok(EntryAuthorization::native(request));
        }
        let provider = provider.ok_or(Error::Unavailable)?;
        if provider.id() != record.policy {
            return Err(Error::Configuration);
        }
        let authorization = provider.authorize_entry(&request)?;
        if authorization.request != request
            || authorization.context == [0; 32]
            || authorization.receipt.is_none()
        {
            return Err(Error::InvalidAuthorization);
        }
        Ok(authorization)
    }
}
