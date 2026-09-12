use crate::transport::*;

/// The tribute offer key derived once from the DKG group threshold signature
/// (Seam F): the secret stays resident, clients encrypt to `public`. Written on
/// the founding DKG connection's `DkgFinalizeTributeOffer`, then read by the
/// offer-decrypt path on other connections. Also carries the resident group
/// threshold signature so the restart fast-path restores the same permanent key.
pub struct DerivedTributeOfferKey {
    secret: Zeroizing<[u8; 32]>,
    pub(in crate::transport) public: [u8; 32],
    group_sig: Zeroizing<Vec<u8>>,
    key_epoch: u64,
    tribute_offer_epoch: u64,
}

impl DerivedTributeOfferKey {
    /// The resident offer secret (never leaves the enclave).
    pub(crate) fn secret(&self) -> &[u8; 32] {
        &self.secret
    }
    /// The offer public key clients encrypt to (registered on-chain at bootstrap).
    pub fn public(&self) -> [u8; 32] {
        self.public
    }
    /// The resident group threshold signature (Seam F output). It never leaves
    /// the enclave unsealed and is persisted only inside sealed restart state.
    pub(crate) fn group_sig(&self) -> &[u8] {
        &self.group_sig
    }
    pub(crate) const fn key_epoch(&self) -> u64 {
        self.key_epoch
    }
    pub(crate) const fn tribute_offer_epoch(&self) -> u64 {
        self.tribute_offer_epoch
    }
    /// Build from explicit founding-finalization or registry-onboarding material,
    /// whose enclave-only path already holds the public component.
    pub(in crate::transport) fn from_parts(
        secret: Zeroizing<[u8; 32]>,
        public: [u8; 32],
        group_sig: Zeroizing<Vec<u8>>,
    ) -> Self {
        Self {
            secret,
            public,
            group_sig,
            key_epoch: 0,
            tribute_offer_epoch: 0,
        }
    }

    pub(in crate::transport) fn with_epochs(
        mut self,
        key_epoch: u64,
        tribute_offer_epoch: u64,
    ) -> Self {
        self.key_epoch = key_epoch;
        self.tribute_offer_epoch = tribute_offer_epoch;
        self
    }
    /// Reconstruct from a resident offer secret + group signature (recomputes the
    /// public key). Used by the seal/unseal boot path to restore the DKG-derived
    /// offer key on restart without re-running the ceremony.
    pub(in crate::transport) fn from_secret_and_group_sig(
        secret: Zeroizing<[u8; 32]>,
        group_sig: Zeroizing<Vec<u8>>,
    ) -> Self {
        let public = crate::crypto::x25519_public(&secret);
        Self::from_parts(secret, public, group_sig)
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        secret: [u8; 32],
        group_sig: Vec<u8>,
        key_epoch: u64,
        tribute_offer_epoch: u64,
    ) -> Self {
        Self::from_secret_and_group_sig(Zeroizing::new(secret), Zeroizing::new(group_sig))
            .with_epochs(key_epoch, tribute_offer_epoch)
    }
}

/// Process-wide, write-once slot for the DKG-derived offer key, shared across
/// every connection thread. `OnceLock` makes the first ceremony's key canonical;
/// a divergent founding finalization is rejected by the enclave request arm. No
/// `StorageHandle` exists in this binary, so std sync primitives apply here.
pub type SharedTributeOfferKey = Arc<OnceLock<DerivedTributeOfferKey>>;

/// The TSEAL sealing key + its policy, or `None` when no confidential key is
/// available. Real `EGETKEY(MRSIGNER)` under `gramine-sgx`; a fixed mock key
/// under `mock`/test (stable across rebuilds, simulating MRSIGNER); nothing under
/// `gramine-direct` prod, where there is no confidential at-rest persistence.
pub(crate) fn sealing_key() -> Option<([u8; 32], KeyPolicy)> {
    if let Ok(k) = crate::gramine::sealing_key_256(true) {
        return Some((k, KeyPolicy::MrSigner));
    }
    #[cfg(any(test, feature = "mock"))]
    {
        Some((crate::seal::MOCK_SEALING_KEY, KeyPolicy::Mock))
    }
    #[cfg(not(any(test, feature = "mock")))]
    {
        None
    }
}

/// Create one owner-only durable sealed blob without replacing existing state.
/// A crash can leave a partial final file, which is deliberately fail-closed:
/// subsequent startup rejects it and never converts corruption into rotation.
pub(crate) fn write_once_0600(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(data)?;
    file.sync_all()?;
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

/// Restore the permanent offer key + group signature from the sealed blob at
/// boot. A missing blob is the only keyless result and is valid only for a fresh
/// identity whose startup context will prove founding/onboarding eligibility.
/// Any existing blob that cannot be read or unsealed is terminal: a permanent
/// key is never recovered, replaced, or regenerated for the same identity.
pub fn unseal_tribute_offer_and_group_sig_on_boot(
    cfg: &EnclaveBootConfig,
    network_binding: outbe_primitives::tee_attestation_v1::NetworkBindingV1,
) -> Result<Option<DerivedTributeOfferKey>, String> {
    let path = cfg.sealed_root_path();
    let blob = match std::fs::read(&path) {
        Ok(blob) => blob,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "read sealed permanent offer key {}: {error}; no recovery or fallback exists",
                path.display()
            ));
        }
    };
    let (key, _policy) = sealing_key().ok_or_else(|| {
        format!(
            "sealed permanent offer key {} exists but no SGX sealing key is available; no recovery or fallback exists",
            path.display()
        )
    })?;
    match unseal_tribute_offer_and_group_sig(&blob, &key, network_binding, cfg.isv_svn) {
        Ok(unsealed) => {
            eprintln!(
                "outbe-tee-enclave: unsealed offer key + group signature <- {} (restart fast-path)",
                path.display()
            );
            Ok(Some(
                DerivedTributeOfferKey::from_secret_and_group_sig(
                    unsealed.tribute_offer_secret,
                    unsealed.group_sig,
                )
                .with_epochs(
                    unsealed.header.key_epoch,
                    unsealed.header.tribute_offer_epoch,
                ),
            ))
        }
        Err(error) => Err(format!(
            "unseal permanent offer key {} failed: {error}; this identity has lost its key and no recovery or fallback exists",
            path.display()
        )),
    }
}

pub(in crate::transport) fn persist_offer_key_required(
    cfg: &EnclaveBootConfig,
    network_binding: outbe_primitives::tee_attestation_v1::NetworkBindingV1,
    derived: &DerivedTributeOfferKey,
) -> Result<(), String> {
    let path = cfg.sealed_root_path();
    let (key, policy) = sealing_key()
        .ok_or_else(|| "production onboarding requires an SGX sealing key".to_string())?;
    let mut nonce = [0u8; 12];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut nonce)
        .map_err(|_| "offer-key seal nonce RNG failed".to_string())?;
    let header = SealHeader {
        format_version: SEAL_FORMAT,
        key_policy: policy,
        isv_svn: cfg.isv_svn,
        key_epoch: derived.key_epoch(),
        tribute_offer_epoch: derived.tribute_offer_epoch(),
        nonce,
    };
    let blob = seal_tribute_offer_and_group_sig(
        derived.secret(),
        derived.group_sig(),
        &key,
        network_binding,
        &header,
    )
    .map_err(|error| format!("seal permanent offer key: {error}"))?;
    match write_once_0600(&path, &blob) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = std::fs::read(&path)
                .map_err(|error| format!("read concurrent sealed offer key: {error}"))?;
            let existing =
                unseal_tribute_offer_and_group_sig(&existing, &key, network_binding, cfg.isv_svn)
                    .map_err(|error| format!("verify existing sealed offer key: {error}"))?;
            if existing.tribute_offer_secret.as_ref() != derived.secret()
                || existing.group_sig.as_slice() != derived.group_sig()
                || existing.header.key_epoch != derived.key_epoch()
                || existing.header.tribute_offer_epoch != derived.tribute_offer_epoch()
            {
                return Err("existing sealed offer key differs from onboarding result".into());
            }
            Ok(())
        }
        Err(error) => Err(format!(
            "write sealed offer key {}: {error}",
            path.display()
        )),
    }
}

/// Durable commit point for the permanent group secret. No caller may publish
/// the key in the process-wide write-once slot until the network-bound seal is
/// safely persisted.
pub(in crate::transport) fn persist_then_activate_offer_key(
    boot: &EnclaveBootConfig,
    network_binding: NetworkBindingV1,
    offer_key: &SharedTributeOfferKey,
    derived: DerivedTributeOfferKey,
) -> Result<(), String> {
    persist_offer_key_required(boot, network_binding, &derived)?;
    if let Err(rejected) = offer_key.set(derived) {
        if offer_key.get().map(DerivedTributeOfferKey::public) != Some(rejected.public()) {
            return Err("offer key divergence after durable persistence".to_string());
        }
    }
    Ok(())
}
