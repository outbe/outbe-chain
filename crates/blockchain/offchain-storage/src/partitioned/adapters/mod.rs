//! Physical datasource adapters. Common orchestration never imports these.

mod codec;
mod collections;
mod memory;
mod mongo;
pub mod rocks;

pub use collections::{CollectionDataSource, ScopeCatalog};
pub use memory::MemoryPartitionDataSource;
pub use mongo::MongoPartitionDataSource;

pub use rocks::{RocksPartitionDataSource, RocksPartitionReadView};
