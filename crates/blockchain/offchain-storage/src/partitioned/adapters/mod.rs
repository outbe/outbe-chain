//! Physical datasource adapters. Common orchestration never imports these.

mod codec;
mod collections;
mod memory;
pub mod rocks;

pub use collections::{CollectionDataSource, ScopeCatalog};
pub use memory::MemoryPartitionDataSource;

pub use rocks::{RocksPartitionDataSource, RocksPartitionReadView};
