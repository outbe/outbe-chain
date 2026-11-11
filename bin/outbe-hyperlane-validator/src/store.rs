//! Local MDBX store of the Hyperlane validator.
//!
//! One environment in its own directory (never the node database). Tables:
//! - `Leaves`: (domain, index) -> message id + origin block.
//! - `Checkpoints`: (domain, index) -> root + 65-byte signature.
//! - `Cursor`: domain -> last processed finalized origin block.
//! - `Meta`: (domain, [`Meta`]) -> bytes.
//!
//! Keys of `Leaves`, `Checkpoints` and `Meta` are `domain << 32 | index` as
//! big-endian `u64`, so a cursor walks one domain in index order. The
//! environment opens in MDBX exclusive mode: a second process cannot open the
//! same directory.

use std::path::Path;

use alloy_primitives::B256;
use eyre::{Context, Result};
use reth_db::{
    cursor::DbCursorRO,
    database::Database,
    mdbx::{create_db, DatabaseArguments},
    table::{Table, TableInfo},
    transaction::{DbTx, DbTxMut},
    ClientVersion, DatabaseEnv, TableSet,
};

use crate::errors::ValidatorError;

macro_rules! table {
    ($name:ident, $key:ty) => {
        #[derive(Debug)]
        pub struct $name;

        impl Table for $name {
            const NAME: &'static str = concat!("HyperlaneValidator", stringify!($name));
            const DUPSORT: bool = false;
            type Key = $key;
            type Value = Vec<u8>;
        }

        impl TableInfo for $name {
            fn name(&self) -> &'static str {
                <Self as Table>::NAME
            }

            fn is_dupsort(&self) -> bool {
                false
            }
        }
    };
}

table!(Leaves, u64);
table!(Checkpoints, u64);
table!(Cursor, u32);
table!(MetaTable, u64);

#[derive(Debug)]
struct Tables;

impl TableSet for Tables {
    fn tables() -> Box<dyn Iterator<Item = Box<dyn TableInfo>>> {
        Box::new(
            [
                Box::new(Leaves) as Box<dyn TableInfo>,
                Box::new(Checkpoints),
                Box::new(Cursor),
                Box::new(MetaTable),
            ]
            .into_iter(),
        )
    }
}

/// A `MerkleTreeHook` leaf as the origin chain inserted it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Leaf {
    pub message_id: B256,
    pub block: u64,
}

/// The root at an index and this validator's signature over that checkpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignedRoot {
    pub root: B256,
    pub signature: [u8; 65],
}

/// Names of the per-domain metadata entries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Meta {
    Announcement = 1,
    Metadata = 2,
    ReorgFlag = 3,
}

fn key(domain: u32, index: u32) -> u64 {
    (u64::from(domain) << 32) | u64::from(index)
}

fn split(key: u64) -> (u32, u32) {
    ((key >> 32) as u32, key as u32)
}

fn encode_leaf(leaf: &Leaf) -> Vec<u8> {
    let mut v = Vec::with_capacity(40);
    v.extend_from_slice(leaf.message_id.as_slice());
    v.extend_from_slice(&leaf.block.to_be_bytes());
    v
}

fn decode_leaf(v: &[u8]) -> Result<Leaf> {
    let v: &[u8; 40] = v
        .try_into()
        .map_err(|_| ValidatorError::CorruptRecord("leaf"))?;
    Ok(Leaf {
        message_id: B256::from_slice(&v[..32]),
        block: u64::from_be_bytes(v[32..].try_into().expect("8 bytes")),
    })
}

fn encode_signed(signed: &SignedRoot) -> Vec<u8> {
    let mut v = Vec::with_capacity(97);
    v.extend_from_slice(signed.root.as_slice());
    v.extend_from_slice(&signed.signature);
    v
}

fn decode_signed(v: &[u8]) -> Result<SignedRoot> {
    let v: &[u8; 97] = v
        .try_into()
        .map_err(|_| ValidatorError::CorruptRecord("checkpoint"))?;
    Ok(SignedRoot {
        root: B256::from_slice(&v[..32]),
        signature: v[32..].try_into().expect("65 bytes"),
    })
}

/// The validator's MDBX environment.
#[derive(Debug)]
pub struct Store {
    db: DatabaseEnv,
}

impl Store {
    /// Opens (and creates) the store in `dir` in exclusive mode.
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        let args = DatabaseArguments::new(ClientVersion::default()).with_exclusive(Some(true));
        let version = args.client_version().clone();
        let mut db = create_db(dir, args).with_context(|| {
            format!(
                "open MDBX store {} (is another process using it?)",
                dir.display()
            )
        })?;
        db.create_and_track_tables_for::<Tables>()?;
        db.record_client_version(version)?;
        Ok(Self { db })
    }

    /// Writes new leaves, their signed roots and the cursor of one domain in
    /// one transaction. Leaves must continue the stored leaves without a gap.
    /// A signed root must have a leaf, and must not replace a different
    /// signed root at the same index.
    pub fn commit_batch(
        &self,
        domain: u32,
        leaves: &[(u32, Leaf)],
        signed: &[(u32, SignedRoot)],
        cursor: u64,
    ) -> Result<()> {
        let tx = self.db.tx_mut()?;
        let mut next = leaf_count(&tx, domain)?;
        for (index, leaf) in leaves {
            if *index != next {
                return Err(ValidatorError::LeafGap {
                    domain,
                    expected: next,
                    got: *index,
                }
                .into());
            }
            tx.put::<Leaves>(key(domain, *index), encode_leaf(leaf))?;
            next += 1;
        }
        put_signed(&tx, domain, signed, next)?;
        tx.put::<Cursor>(domain, cursor.to_be_bytes().to_vec())?;
        tx.commit()?;
        Ok(())
    }

    /// Writes signed roots for leaves that are already stored (backfill).
    pub fn put_checkpoints(&self, domain: u32, signed: &[(u32, SignedRoot)]) -> Result<()> {
        let tx = self.db.tx_mut()?;
        let count = leaf_count(&tx, domain)?;
        put_signed(&tx, domain, signed, count)?;
        tx.commit()?;
        Ok(())
    }

    pub fn leaf(&self, domain: u32, index: u32) -> Result<Option<Leaf>> {
        let tx = self.db.tx()?;
        tx.get::<Leaves>(key(domain, index))?
            .map(|v| decode_leaf(&v))
            .transpose()
    }

    pub fn checkpoint(&self, domain: u32, index: u32) -> Result<Option<SignedRoot>> {
        let tx = self.db.tx()?;
        tx.get::<Checkpoints>(key(domain, index))?
            .map(|v| decode_signed(&v))
            .transpose()
    }

    /// Number of stored leaves of `domain`. Leaves have no gaps, so this is
    /// also the next leaf index.
    pub fn leaf_count(&self, domain: u32) -> Result<u32> {
        leaf_count(&self.db.tx()?, domain)
    }

    /// Highest index of `domain` that has a signed root.
    pub fn latest_checkpoint_index(&self, domain: u32) -> Result<Option<u32>> {
        last_index::<Checkpoints>(&self.db.tx()?, domain)
    }

    pub fn cursor(&self, domain: u32) -> Result<Option<u64>> {
        let tx = self.db.tx()?;
        tx.get::<Cursor>(domain)?
            .map(|v| {
                let bytes: [u8; 8] = v
                    .as_slice()
                    .try_into()
                    .map_err(|_| ValidatorError::CorruptRecord("cursor"))?;
                Ok(u64::from_be_bytes(bytes))
            })
            .transpose()
    }

    /// Calls `f` for every leaf of `domain` in index order (tree rebuild).
    pub fn for_each_leaf(&self, domain: u32, mut f: impl FnMut(u32, Leaf)) -> Result<()> {
        let tx = self.db.tx()?;
        let mut cursor = tx.cursor_read::<Leaves>()?;
        for row in cursor.walk(Some(key(domain, 0)))? {
            let (k, v) = row?;
            let (d, index) = split(k);
            if d != domain {
                break;
            }
            f(index, decode_leaf(&v)?);
        }
        Ok(())
    }

    /// At most `limit` leaves of `domain` from index `from`, in index order,
    /// each with its signed root if one is stored (backfill).
    pub fn leaves_from(
        &self,
        domain: u32,
        from: u32,
        limit: usize,
    ) -> Result<Vec<(u32, Leaf, Option<SignedRoot>)>> {
        let tx = self.db.tx()?;
        let mut out = Vec::new();
        for row in tx.cursor_read::<Leaves>()?.walk(Some(key(domain, from)))? {
            let (k, v) = row?;
            let (d, index) = split(k);
            if d != domain || out.len() == limit {
                break;
            }
            let signed = tx
                .get::<Checkpoints>(k)?
                .map(|v| decode_signed(&v))
                .transpose()?;
            out.push((index, decode_leaf(&v)?, signed));
        }
        Ok(out)
    }

    /// Lowest index of `domain` that has a leaf but no signed root (backfill).
    pub fn first_unsigned_index(&self, domain: u32) -> Result<Option<u32>> {
        let tx = self.db.tx()?;
        let count = leaf_count(&tx, domain)?;
        // Signed indexes are a subset of the leaf indexes 0..count. Walk them
        // in order: the first index that is missing is the gap.
        let mut expected = 0u32;
        let mut cursor = tx.cursor_read::<Checkpoints>()?;
        for row in cursor.walk(Some(key(domain, 0)))? {
            let (d, index) = split(row?.0);
            if d != domain || index != expected {
                break;
            }
            expected += 1;
        }
        Ok((expected < count).then_some(expected))
    }

    pub fn meta(&self, domain: u32, name: Meta) -> Result<Option<Vec<u8>>> {
        Ok(self.db.tx()?.get::<MetaTable>(key(domain, name as u32))?)
    }

    pub fn set_meta(&self, domain: u32, name: Meta, value: &[u8]) -> Result<()> {
        let tx = self.db.tx_mut()?;
        tx.put::<MetaTable>(key(domain, name as u32), value.to_vec())?;
        tx.commit()?;
        Ok(())
    }

    pub fn delete_meta(&self, domain: u32, name: Meta) -> Result<()> {
        let tx = self.db.tx_mut()?;
        tx.delete::<MetaTable>(key(domain, name as u32), None)?;
        tx.commit()?;
        Ok(())
    }

    /// Domains that have a cursor or metadata, in ascending order.
    pub fn domains(&self) -> Result<Vec<u32>> {
        let tx = self.db.tx()?;
        let mut domains = Vec::new();
        for row in tx.cursor_read::<Cursor>()?.walk(None)? {
            domains.push(row?.0);
        }
        for row in tx.cursor_read::<MetaTable>()?.walk(None)? {
            domains.push(split(row?.0).0);
        }
        domains.sort_unstable();
        domains.dedup();
        Ok(domains)
    }
}

fn leaf_count(tx: &impl DbTx, domain: u32) -> Result<u32> {
    Ok(last_index::<Leaves>(tx, domain)?.map_or(0, |i| i + 1))
}

fn last_index<T: Table<Key = u64>>(tx: &impl DbTx, domain: u32) -> Result<Option<u32>> {
    let mut cursor = tx.cursor_read::<T>()?;
    // The last key below the next domain belongs to this domain, if any.
    let row = match cursor.seek(key(domain, 0).saturating_add(1 << 32))? {
        Some(_) => cursor.prev()?,
        None => cursor.last()?,
    };
    Ok(row
        .map(|(k, _)| split(k))
        .filter(|(d, _)| *d == domain)
        .map(|(_, index)| index))
}

/// Writes signed roots. Every index must have a leaf (`index < leaf_count`).
/// An identical stored root is kept. A different stored root is a conflict:
/// one index never gets two signatures.
fn put_signed(
    tx: &(impl DbTx + DbTxMut),
    domain: u32,
    signed: &[(u32, SignedRoot)],
    leaf_count: u32,
) -> Result<()> {
    for (index, value) in signed {
        if *index >= leaf_count {
            return Err(ValidatorError::CheckpointWithoutLeaf {
                domain,
                index: *index,
            }
            .into());
        }
        let k = key(domain, *index);
        match tx.get::<Checkpoints>(k)? {
            Some(stored) if decode_signed(&stored)? == *value => {}
            Some(_) => {
                return Err(ValidatorError::CheckpointConflict {
                    domain,
                    index: *index,
                }
                .into())
            }
            None => tx.put::<Checkpoints>(k, encode_signed(value))?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(n: u8) -> Leaf {
        Leaf {
            message_id: B256::repeat_byte(n),
            block: u64::from(n) * 10,
        }
    }

    fn signed(n: u8) -> SignedRoot {
        SignedRoot {
            root: B256::repeat_byte(n),
            signature: [n; 65],
        }
    }

    #[test]
    fn batch_write_and_reads() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(store.leaf_count(7).unwrap(), 0);
        assert_eq!(store.latest_checkpoint_index(7).unwrap(), None);

        store
            .commit_batch(7, &[(0, leaf(1)), (1, leaf(2))], &[(1, signed(9))], 100)
            .unwrap();
        assert_eq!(store.leaf(7, 1).unwrap(), Some(leaf(2)));
        assert_eq!(store.checkpoint(7, 1).unwrap(), Some(signed(9)));
        assert_eq!(store.checkpoint(7, 0).unwrap(), None);
        assert_eq!(store.leaf_count(7).unwrap(), 2);
        assert_eq!(store.latest_checkpoint_index(7).unwrap(), Some(1));
        assert_eq!(store.cursor(7).unwrap(), Some(100));
        assert_eq!(store.first_unsigned_index(7).unwrap(), Some(0));

        // Backfill index 0: no gap is left.
        store.put_checkpoints(7, &[(0, signed(8))]).unwrap();
        assert_eq!(store.first_unsigned_index(7).unwrap(), None);
        // The same value again is a no-op.
        store.put_checkpoints(7, &[(0, signed(8))]).unwrap();
    }

    #[test]
    fn rejected_batch_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store
            .commit_batch(1, &[(0, leaf(1))], &[(0, signed(1))], 5)
            .unwrap();

        // Gap: index 2 after 0.
        let err = store
            .commit_batch(1, &[(1, leaf(2)), (3, leaf(3))], &[], 6)
            .unwrap_err();
        assert!(err.to_string().contains("leaf gap"), "{err}");
        // A second signature with a different root on index 0.
        let err = store
            .commit_batch(1, &[(1, leaf(2))], &[(0, signed(2))], 6)
            .unwrap_err();
        assert!(err.to_string().contains("different root"), "{err}");
        // A signature without a leaf.
        assert!(store.put_checkpoints(1, &[(5, signed(5))]).is_err());

        // Nothing of the rejected batches is stored.
        assert_eq!(store.leaf_count(1).unwrap(), 1);
        assert_eq!(store.leaf(1, 1).unwrap(), None);
        assert_eq!(store.checkpoint(1, 0).unwrap(), Some(signed(1)));
        assert_eq!(store.cursor(1).unwrap(), Some(5));
    }

    #[test]
    fn domains_do_not_mix() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store
            .commit_batch(2, &[(0, leaf(20)), (1, leaf(21))], &[(0, signed(20))], 1)
            .unwrap();
        store
            .commit_batch(1, &[(0, leaf(10)), (1, leaf(11)), (2, leaf(12))], &[], 1)
            .unwrap();
        store
            .commit_batch(3, &[(0, leaf(30))], &[(0, signed(30))], 1)
            .unwrap();

        let mut seen = Vec::new();
        store.for_each_leaf(1, |i, l| seen.push((i, l))).unwrap();
        assert_eq!(seen, vec![(0, leaf(10)), (1, leaf(11)), (2, leaf(12))]);
        assert_eq!(store.leaf_count(2).unwrap(), 2);
        assert_eq!(store.latest_checkpoint_index(1).unwrap(), None);
        assert_eq!(store.latest_checkpoint_index(2).unwrap(), Some(0));
        assert_eq!(store.latest_checkpoint_index(3).unwrap(), Some(0));
        assert_eq!(store.first_unsigned_index(2).unwrap(), Some(1));
        assert_eq!(store.first_unsigned_index(3).unwrap(), None);
        assert_eq!(store.domains().unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn meta_set_get_delete() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(store.meta(4, Meta::ReorgFlag).unwrap(), None);
        store.set_meta(4, Meta::ReorgFlag, b"reorg").unwrap();
        store.set_meta(4, Meta::Announcement, b"a").unwrap();
        assert_eq!(
            store.meta(4, Meta::ReorgFlag).unwrap().as_deref(),
            Some(&b"reorg"[..])
        );
        assert_eq!(store.domains().unwrap(), vec![4]);
        store.delete_meta(4, Meta::ReorgFlag).unwrap();
        assert_eq!(store.meta(4, Meta::ReorgFlag).unwrap(), None);
        assert_eq!(
            store.meta(4, Meta::Announcement).unwrap().as_deref(),
            Some(&b"a"[..])
        );
    }

    #[test]
    fn data_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = Store::open(dir.path()).unwrap();
            store
                .commit_batch(9, &[(0, leaf(1))], &[(0, signed(1))], 42)
                .unwrap();
        }
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(store.checkpoint(9, 0).unwrap(), Some(signed(1)));
        assert_eq!(store.cursor(9).unwrap(), Some(42));
    }

    #[test]
    fn second_open_fails_while_open() {
        let dir = tempfile::tempdir().unwrap();
        let _first = Store::open(dir.path()).unwrap();
        // MDBX exclusive mode: the OS lock rejects the second open (EAGAIN).
        assert!(Store::open(dir.path()).is_err());
    }
}
