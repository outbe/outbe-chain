//! Logical partition addresses, without physical backend encodings.

use crate::{Namespace, StorageError};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum PartitionId {
    Shared,
    Numbered { family: String, index: u32 },
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct StorageScope {
    pub domain: String,
    pub partition: PartitionId,
}

impl StorageScope {
    pub fn validate(&self) -> Result<(), StorageError> {
        Self::new(&self.domain, self.partition.clone()).map(|_| ())
    }
    pub fn shared(domain: &str) -> Result<Self, StorageError> {
        Self::new(domain, PartitionId::Shared)
    }
    pub fn numbered(domain: &str, family: &str, index: u32) -> Result<Self, StorageError> {
        Self::new(
            domain,
            PartitionId::Numbered {
                family: family.to_owned(),
                index,
            },
        )
    }
    pub fn new(domain: &str, partition: PartitionId) -> Result<Self, StorageError> {
        Namespace::new(domain)?;
        if domain.contains("__") {
            return Err(StorageError::InvalidArgument(
                "domain contains reserved separator".into(),
            ));
        }
        if let PartitionId::Numbered { family, .. } = &partition {
            Namespace::new(family.replace('-', "_"))?;
            if family.is_empty()
                || !family.as_bytes()[0].is_ascii_lowercase()
                || !family
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
            {
                return Err(StorageError::InvalidArgument(
                    "invalid partition family".into(),
                ));
            }
        }
        Ok(Self {
            domain: domain.to_owned(),
            partition,
        })
    }
}
