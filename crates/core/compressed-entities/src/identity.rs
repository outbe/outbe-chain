use outbe_common::WorldwideDay;
use outbe_primitives::storage::types::StorageKey;
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use thiserror::Error;

/// Exact logical body identity: WWD BE4 followed by one complete 32-byte digest.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EntityId36([u8; Self::LEN]);

impl EntityId36 {
    /// Canonical byte length at Rust and ABI boundaries.
    pub const LEN: usize = 36;

    /// Builds an identity without truncating either the day or digest.
    #[must_use]
    pub fn new(worldwide_day: WorldwideDay, digest: [u8; 32]) -> Self {
        let mut bytes = [0_u8; Self::LEN];
        bytes[..4].copy_from_slice(&worldwide_day.value().to_be_bytes());
        bytes[4..].copy_from_slice(&digest);
        Self(bytes)
    }

    /// Returns the exact canonical bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; Self::LEN] {
        &self.0
    }

    /// Returns the immutable partition/day prefix.
    #[must_use]
    pub fn worldwide_day(self) -> WorldwideDay {
        WorldwideDay::new(u32::from_be_bytes(
            self.0[..4].try_into().expect("fixed prefix"),
        ))
    }

    /// Returns the complete domain digest.
    #[must_use]
    pub fn digest(self) -> [u8; 32] {
        self.0[4..].try_into().expect("fixed digest")
    }

    /// Consumes the identity into its exact bytes.
    #[must_use]
    pub const fn into_bytes(self) -> [u8; Self::LEN] {
        self.0
    }
}

impl fmt::Display for EntityId36 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&hex::encode(self.0))
    }
}

impl StorageKey for EntityId36 {
    fn key_bytes(&self) -> Vec<u8> {
        self.0.to_vec()
    }
}

impl Serialize for EntityId36 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for EntityId36 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct EntityIdVisitor;

        impl<'de> de::Visitor<'de> for EntityIdVisitor {
            type Value = EntityId36;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("exactly 36 entity identity bytes")
            }

            fn visit_bytes<E>(self, value: &[u8]) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                EntityId36::try_from(value).map_err(E::custom)
            }

            fn visit_byte_buf<E>(self, value: Vec<u8>) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                self.visit_bytes(&value)
            }
        }

        deserializer.deserialize_bytes(EntityIdVisitor)
    }
}

impl TryFrom<&[u8]> for EntityId36 {
    type Error = EntityIdError;

    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        let bytes: [u8; Self::LEN] =
            value.try_into().map_err(|_| EntityIdError::InvalidLength {
                actual: value.len(),
            })?;
        Ok(Self(bytes))
    }
}

/// Invalid canonical entity identity.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum EntityIdError {
    /// ABI/storage input was not exactly 36 bytes.
    #[error("entity ID must be exactly 36 bytes, got {actual}")]
    InvalidLength { actual: usize },
}
