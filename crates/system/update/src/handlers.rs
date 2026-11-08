//! Upgrade migration handler types and registry lookup.

use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::Result;

use crate::state::ScheduledUpdateInfo;
use crate::ProtocolVersion;

/// Static handler table entry type.
pub type UpgradeHandlers = &'static [&'static dyn UpgradeHandler];

/// Migration invoked when a scheduled update reaches its activation height.
pub trait UpgradeHandler: Send + Sync {
    /// The protocol version which should trigger this handler.
    fn version(&self) -> ProtocolVersion;

    /// A human-readable label for the handler.
    fn label(&self) -> &'static str;

    /// The handler logic.
    fn handle(&self, ctx: &BlockRuntimeContext, scheduled: &ScheduledUpdateInfo) -> Result<()>;
}

/// Read-only view over a compile-time handler table.
#[derive(Clone, Copy)]
pub struct UpgradeHandlerRegistry {
    handlers: UpgradeHandlers,
}

impl UpgradeHandlerRegistry {
    /// Builds a registry from a static handler table.
    pub const fn new(handlers: UpgradeHandlers) -> Self {
        Self { handlers }
    }

    /// Returns all handlers registered for `version`.
    pub fn lookup(
        &self,
        version: ProtocolVersion,
    ) -> impl Iterator<Item = &'static dyn UpgradeHandler> {
        self.handlers
            .iter()
            .copied()
            .filter(move |handler| handler.version() == version)
    }
}

/// Active upgrade handlers for this node binary.
///
/// Append entries when a protocol version requires deterministic storage
/// migration at activation height. Versions without a handler activate as
/// version-only switches.
pub static ACTIVE_UPGRADE_HANDLERS: UpgradeHandlers = &[];

static REGISTRY: UpgradeHandlerRegistry = UpgradeHandlerRegistry::new(ACTIVE_UPGRADE_HANDLERS);

/// Returns the compile-time upgrade handler registry for executor and startup wiring.
#[must_use]
pub fn registry() -> &'static UpgradeHandlerRegistry {
    &REGISTRY
}
