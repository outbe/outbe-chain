//! Store the active protocol version and its activation history.

use outbe_primitives::error::Result;

use crate::errors::UpdateError;
use crate::schema::Update;
use crate::ProtocolVersion;

impl Update<'_> {
    /// Reads the active protocol version (`0` = baseline / pre-upgrade chain).
    pub fn get_active_version(&self) -> Result<ProtocolVersion> {
        self.active_version.read()
    }

    /// Reads the activation height of the current active version.
    pub fn get_active_version_height(&self) -> Result<u64> {
        self.active_version_height.read()
    }

    /// Reads the version recorded at `height` (`0` when no upgrade was recorded there).
    pub fn version_at_height(&self, height: u64) -> Result<ProtocolVersion> {
        self.version_history.read(&height)
    }

    /// Writes the active protocol version and records it in `version_history`.
    pub fn set_active_version(&mut self, version: ProtocolVersion, height: u64) -> Result<()> {
        if version.is_zero() {
            return Err(UpdateError::InvalidVersion.into());
        }
        self.active_version.write(version)?;
        self.active_version_height.write(height)?;
        self.version_history.write(&height, version)?;
        Ok(())
    }
}
