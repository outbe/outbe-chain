use super::*;
#[derive(Default)]
/// Resident ceremony map; dropping sessions destroys their owned secret scalars.
pub struct DkgSessionStore {
    sessions: BTreeMap<CeremonyKey, DkgSession>,
}

impl DkgSessionStore {
    /// Create an empty RAM-only ceremony store.
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn open(
        &mut self,
        id: CeremonyKey,
        info: CeremonyInfo,
        signing_key: PrivKey,
        enc_secret: &[u8; 32],
        recipient_enc_keys: BTreeMap<PubKey, [u8; 32]>,
    ) -> Result<()> {
        if self.sessions.contains_key(&id) {
            return Err(DkgError::CeremonyAlreadyOpen.into());
        }
        self.sessions.insert(
            id,
            DkgSession::new(info, signing_key, enc_secret, recipient_enc_keys)?,
        );
        Ok(())
    }

    /// Borrow an existing resident ceremony or return SessionMissing.
    pub fn get_mut(&mut self, id: &CeremonyKey) -> Result<&mut DkgSession> {
        self.sessions
            .get_mut(id)
            .ok_or_else(|| DkgError::SessionMissing.into())
    }

    /// Remove a ceremony; dropping the returned session zeroizes its owned secrets.
    pub fn remove(&mut self, id: &CeremonyKey) -> Option<DkgSession> {
        self.sessions.remove(id)
    }

    /// Return the number of resident ceremonies.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Report whether any ceremony remains resident.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

impl std::fmt::Debug for DkgSessionStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DkgSessionStore([REDACTED])")
    }
}
