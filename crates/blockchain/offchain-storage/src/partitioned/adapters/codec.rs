use super::super::{PartitionId, StorageScope};
use crate::{Namespace, StorageError};
use std::collections::BTreeSet;

pub(super) fn namespace(
    scope: &StorageScope,
    logical: &Namespace,
) -> Result<Namespace, StorageError> {
    let partition = match &scope.partition {
        PartitionId::Shared => "shared".to_owned(),
        PartitionId::Numbered { family, index } => format!("{}_{index}", family.replace('-', "_")),
    };
    Namespace::new(format!(
        "{}__{partition}__{}",
        scope.domain,
        logical.as_str()
    ))
}

pub(super) fn scopes(names: Vec<String>, domain: &str) -> Result<Vec<StorageScope>, StorageError> {
    let mut result = BTreeSet::new();
    for name in names {
        let mut parts = name.splitn(3, "__");
        if parts.next() != Some(domain) {
            continue;
        }
        let Some(partition) = parts.next() else {
            continue;
        };
        let logical = parts.next().ok_or_else(|| {
            StorageError::Corruption("missing scoped collection namespace".into())
        })?;
        let logical = Namespace::new(logical)?;
        let scope = if partition == "shared" {
            StorageScope::shared(domain)?
        } else {
            let (family, index) = partition
                .rsplit_once('_')
                .ok_or_else(|| StorageError::Corruption("invalid numbered collection".into()))?;
            StorageScope::numbered(
                domain,
                &family.replace('_', "-"),
                index.parse().map_err(|_| {
                    StorageError::Corruption("invalid collection partition number".into())
                })?,
            )?
        };
        if namespace(&scope, &logical)?.as_str() != name {
            return Err(StorageError::Corruption(
                "non-canonical scoped collection".into(),
            ));
        }
        result.insert(scope);
    }
    Ok(result.into_iter().collect())
}

pub(super) fn namespaces(
    names: Vec<String>,
    scope: &StorageScope,
) -> Result<Vec<Namespace>, StorageError> {
    scope.validate()?;
    let sample = namespace(scope, &Namespace::new("placeholder")?)?;
    let prefix = sample
        .as_str()
        .strip_suffix("placeholder")
        .expect("namespace suffix");
    names
        .into_iter()
        .filter(|name| name.starts_with(prefix))
        .map(Namespace::new)
        .collect()
}
