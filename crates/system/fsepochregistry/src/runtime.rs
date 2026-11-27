//! Runtime implementation for the FsEpochRegistry native precompile.

use alloy_primitives::{keccak256, Address, Bytes, B256};
use outbe_primitives::error::Result;

use crate::bls::{decode_eip2537_g2, verify_eip2537_min_sig, EIP2537_G1_LEN, EIP2537_G2_LEN};
use crate::errors::FsEpochRegistryError;
use crate::precompile::IFsEpochRegistry;
use crate::schema::FsEpochRegistryContract;

/// Canonical encoding version byte prefixed to every epoch digest preimage.
pub const ENCODING_VERSION: u8 = 1;

/// Key material and validity window of a stored epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpochKey {
    pub pk_att: Vec<u8>,
    pub dst: Vec<u8>,
    pub start_wwd: u32,
    pub end_wwd_exclusive: u32,
}

/// Summary of the registry's head epoch (served by `fsEpoch_head`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FsEpochHead {
    pub anchored: bool,
    pub head: u32,
    pub start_wwd: u32,
    pub digest: B256,
}

/// Computes the canonical epoch digest:
/// `keccak256(u8(1) || u32be(epoch) || pkAtt(256) || u8(len(dst)) || dst || u32be(startWwd) || membersHash(32) || prevHash(32))`.
pub fn digest_of(
    epoch: u32,
    pk_att: &[u8],
    dst: &[u8],
    start_wwd: u32,
    members_hash: B256,
    prev_hash: B256,
) -> Result<B256> {
    require_lengths(pk_att.len(), dst.len())?;
    let mut buf = Vec::with_capacity(1 + 4 + EIP2537_G2_LEN + 1 + dst.len() + 4 + 32 + 32);
    buf.push(ENCODING_VERSION);
    buf.extend_from_slice(&epoch.to_be_bytes());
    buf.extend_from_slice(pk_att);
    buf.push(dst.len() as u8);
    buf.extend_from_slice(dst);
    buf.extend_from_slice(&start_wwd.to_be_bytes());
    buf.extend_from_slice(members_hash.as_slice());
    buf.extend_from_slice(prev_hash.as_slice());
    Ok(keccak256(&buf))
}

fn require_lengths(pk_att_len: usize, dst_len: usize) -> Result<()> {
    if pk_att_len != EIP2537_G2_LEN {
        return Err(FsEpochRegistryError::InvalidKeyLength.into());
    }
    if dst_len == 0 || dst_len > 255 {
        return Err(FsEpochRegistryError::InvalidDst.into());
    }
    Ok(())
}

fn require_well_formed(pk_att: &[u8], dst: &[u8]) -> Result<()> {
    require_lengths(pk_att.len(), dst.len())?;
    if decode_eip2537_g2(pk_att).is_none() {
        return Err(FsEpochRegistryError::InvalidKey.into());
    }
    Ok(())
}

impl FsEpochRegistryContract<'_> {
    /// Seeds the initial admin directly (for genesis / tests).
    pub fn bootstrap_admin(&mut self, admin: Address) -> Result<()> {
        if admin == Address::ZERO {
            return Err(FsEpochRegistryError::ZeroAddress.into());
        }
        self.admin.write(admin)
    }

    fn require_admin(&self, caller: Address) -> Result<()> {
        let admin = self.admin.read()?;
        if admin == Address::ZERO || caller != admin {
            return Err(FsEpochRegistryError::NotAdmin.into());
        }
        Ok(())
    }

    fn require_known(&self, epoch: u32) -> Result<()> {
        if !self.anchored.read()? || epoch > self.head.read()? {
            return Err(FsEpochRegistryError::EpochUnknown { epoch }.into());
        }
        Ok(())
    }

    fn store_epoch(&mut self, epoch: u32, entry: &IFsEpochRegistry::Epoch) -> Result<()> {
        self.epoch_pk_att
            .bytes(&epoch)
            .write(entry.pkAtt.as_ref())?;
        self.epoch_dst.bytes(&epoch).write(entry.dst.as_ref())?;
        self.epoch_start_wwd.write(&epoch, entry.startWwd)?;
        self.epoch_members_hash.write(&epoch, entry.membersHash)?;
        self.epoch_prev_hash.write(&epoch, entry.prevHash)?;
        Ok(())
    }

    fn read_stored_epoch(&self, epoch: u32) -> Result<IFsEpochRegistry::Epoch> {
        Ok(IFsEpochRegistry::Epoch {
            pkAtt: Bytes::from(self.epoch_pk_att.bytes(&epoch).read()?),
            dst: Bytes::from(self.epoch_dst.bytes(&epoch).read()?),
            startWwd: self.epoch_start_wwd.read(&epoch)?,
            membersHash: self.epoch_members_hash.read(&epoch)?,
            prevHash: self.epoch_prev_hash.read(&epoch)?,
        })
    }

    /// Installs epoch 0 (admin only, once only; `prevHash` must be zero).
    pub fn anchor(&mut self, caller: Address, epoch0: &IFsEpochRegistry::Epoch) -> Result<()> {
        self.require_admin(caller)?;
        if self.anchored.read()? {
            return Err(FsEpochRegistryError::AlreadyAnchored.into());
        }
        require_well_formed(epoch0.pkAtt.as_ref(), epoch0.dst.as_ref())?;
        if epoch0.prevHash != B256::ZERO {
            return Err(FsEpochRegistryError::BadPrevHash { epoch: 0 }.into());
        }
        let digest = digest_of(
            0,
            epoch0.pkAtt.as_ref(),
            epoch0.dst.as_ref(),
            epoch0.startWwd,
            epoch0.membersHash,
            epoch0.prevHash,
        )?;

        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.store_epoch(0, epoch0)?;
            self.head.write(0)?;
            self.anchored.write(true)?;
            self.emit(IFsEpochRegistry::EpochAdvanced {
                epoch: 0,
                startWwd: epoch0.startWwd,
                digest,
            })?;
            Ok(())
        })
    }

    /// Truncates the epoch chain to `from - 1` and reserves the next append for the admin.
    pub fn truncate(&mut self, caller: Address, from: u32) -> Result<()> {
        self.require_admin(caller)?;
        let anchored = self.anchored.read()?;
        let head = self.head.read()?;
        if !anchored || from == 0 || from > head {
            return Err(FsEpochRegistryError::EpochUnknown { epoch: from }.into());
        }
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.head.write(from - 1)?;
            self.admin_next.write(true)?;
            self.emit(IFsEpochRegistry::EpochsTruncated { from })?;
            Ok(())
        })
    }

    /// Initiates a two-step admin transfer.
    pub fn initiate_admin_transfer(&mut self, caller: Address, new_admin: Address) -> Result<()> {
        self.require_admin(caller)?;
        if new_admin == Address::ZERO {
            return Err(FsEpochRegistryError::ZeroAddress.into());
        }
        let current = self.admin.read()?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.pending_admin.write(new_admin)?;
            self.emit(IFsEpochRegistry::AdminTransferInitiated {
                from: current,
                to: new_admin,
            })?;
            Ok(())
        })
    }

    /// Completes a two-step admin transfer.
    pub fn accept_admin_transfer(&mut self, caller: Address) -> Result<()> {
        let pending = self.pending_admin.read()?;
        if pending == Address::ZERO || caller != pending {
            return Err(FsEpochRegistryError::NotAdmin.into());
        }
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.admin.write(pending)?;
            self.pending_admin.write(Address::ZERO)?;
            self.emit(IFsEpochRegistry::AdminTransferCompleted { newAdmin: pending })?;
            Ok(())
        })
    }

    /// Appends signed epochs in order from `head + 1`, tolerating an exact stored prefix.
    pub fn advance(
        &mut self,
        caller: Address,
        chain: &[IFsEpochRegistry::SignedEpoch],
    ) -> Result<()> {
        if !self.anchored.read()? {
            return Err(FsEpochRegistryError::NotAnchored.into());
        }
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            for signed in chain {
                let e = signed.epoch;
                let entry = &signed.entry;
                let digest = digest_of(
                    e,
                    entry.pkAtt.as_ref(),
                    entry.dst.as_ref(),
                    entry.startWwd,
                    entry.membersHash,
                    entry.prevHash,
                )?;
                let current_head = self.head.read()?;
                if e <= current_head {
                    let stored = self.read_stored_epoch(e)?;
                    let stored_digest = digest_of(
                        e,
                        stored.pkAtt.as_ref(),
                        stored.dst.as_ref(),
                        stored.startWwd,
                        stored.membersHash,
                        stored.prevHash,
                    )?;
                    if digest != stored_digest {
                        return Err(FsEpochRegistryError::EpochMismatch { epoch: e }.into());
                    }
                    continue;
                }
                if e != current_head.saturating_add(1) {
                    return Err(FsEpochRegistryError::EpochGap {
                        epoch: e,
                        head: current_head,
                    }
                    .into());
                }
                if self.admin_next.read()? {
                    self.require_admin(caller)?;
                    self.admin_next.write(false)?;
                }
                let prev = self.read_stored_epoch(current_head)?;
                require_well_formed(entry.pkAtt.as_ref(), entry.dst.as_ref())?;
                let prev_digest = digest_of(
                    current_head,
                    prev.pkAtt.as_ref(),
                    prev.dst.as_ref(),
                    prev.startWwd,
                    prev.membersHash,
                    prev.prevHash,
                )?;
                if entry.prevHash != prev_digest {
                    return Err(FsEpochRegistryError::BadPrevHash { epoch: e }.into());
                }
                if entry.startWwd <= prev.startWwd {
                    return Err(FsEpochRegistryError::StartWwdNotIncreasing { epoch: e }.into());
                }
                if signed.sigPrev.len() != EIP2537_G1_LEN
                    || !verify_eip2537_min_sig(
                        prev.pkAtt.as_ref(),
                        digest.as_slice(),
                        signed.sigPrev.as_ref(),
                        prev.dst.as_ref(),
                    )
                {
                    return Err(FsEpochRegistryError::BadEpochSignature { epoch: e }.into());
                }
                self.store_epoch(e, entry)?;
                self.head.write(e)?;
                self.emit(IFsEpochRegistry::EpochAdvanced {
                    epoch: e,
                    startWwd: entry.startWwd,
                    digest,
                })?;
            }
            Ok(())
        })
    }

    // -----------------------------------------------------------------------
    // Views
    // -----------------------------------------------------------------------

    /// Key material and validity window of stored epoch `epoch`.
    pub fn key_for(&self, epoch: u32) -> Result<EpochKey> {
        self.require_known(epoch)?;
        let pk_att = self.epoch_pk_att.bytes(&epoch).read()?;
        let dst = self.epoch_dst.bytes(&epoch).read()?;
        let start_wwd = self.epoch_start_wwd.read(&epoch)?;
        let head = self.head.read()?;
        let end_wwd_exclusive = if epoch < head {
            self.epoch_start_wwd.read(&(epoch + 1))?
        } else {
            0
        };
        Ok(EpochKey {
            pk_att,
            dst,
            start_wwd,
            end_wwd_exclusive,
        })
    }

    /// Canonical digest of stored epoch `epoch`.
    pub fn digest_at(&self, epoch: u32) -> Result<B256> {
        self.require_known(epoch)?;
        let stored = self.read_stored_epoch(epoch)?;
        digest_of(
            epoch,
            stored.pkAtt.as_ref(),
            stored.dst.as_ref(),
            stored.startWwd,
            stored.membersHash,
            stored.prevHash,
        )
    }

    /// Reads the head summary served by the `fsEpoch_head` RPC method.
    pub fn head_info(&self) -> Result<FsEpochHead> {
        if !self.anchored.read()? {
            return Ok(FsEpochHead::default());
        }
        let head = self.head.read()?;
        let start_wwd = self.epoch_start_wwd.read(&head)?;
        let digest = self.digest_at(head)?;
        Ok(FsEpochHead {
            anchored: true,
            head,
            start_wwd,
            digest,
        })
    }
}
