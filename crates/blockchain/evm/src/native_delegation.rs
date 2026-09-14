//! EIP-7702 delegations to native routes execute empty code, not the persisted
//! account marker. All frame accounting and completion remain owned by Revm.

use revm::{
    context::FrameStack,
    context_interface::{context::ContextError, Cfg, ContextTr, JournalTr},
    handler::{
        evm::{ContextDbError, FrameInitResult},
        EthFrame, EvmTr, FrameInitOrResult, FrameResult,
    },
    interpreter::{interpreter_action::FrameInit, FrameInput},
    primitives::hardfork::SpecId,
    state::Bytecode,
};

/// Scoped execution adapter; never changes account code or precompile dispatch.
pub(crate) struct NativeDelegationEvm<E>(pub(crate) E);
impl<E: EvmTr<Frame = EthFrame>> EvmTr for NativeDelegationEvm<E> {
    type Context = E::Context;
    type Instructions = E::Instructions;
    type Precompiles = E::Precompiles;
    type Frame = EthFrame;
    fn all(
        &self,
    ) -> (
        &Self::Context,
        &Self::Instructions,
        &Self::Precompiles,
        &FrameStack<Self::Frame>,
    ) {
        self.0.all()
    }
    fn all_mut(
        &mut self,
    ) -> (
        &mut Self::Context,
        &mut Self::Instructions,
        &mut Self::Precompiles,
        &mut FrameStack<Self::Frame>,
    ) {
        self.0.all_mut()
    }
    fn frame_init(
        &mut self,
        mut init: FrameInit,
    ) -> Result<FrameInitResult<'_, Self::Frame>, ContextDbError<Self::Context>> {
        let spec: SpecId = self.0.ctx_ref().cfg().spec().into();
        if spec.is_enabled_in(SpecId::PRAGUE) {
            if let FrameInput::Call(inputs) = &mut init.frame_input {
                // A direct native call keeps its normal ABI dispatch. The route
                // namespace includes the reserved stablecoin class, independent
                // of whether an individual token has been issued.
                if crate::precompile_routes::resolve(&inputs.bytecode_address).is_none() {
                    // Inspect the original account, not resolved bytecode or
                    // target_address (which differs for CALLCODE/DELEGATECALL).
                    let delegate = {
                        let account = self
                            .0
                            .ctx_mut()
                            .journal_mut()
                            .load_account_with_code(inputs.bytecode_address)
                            .map_err(ContextError::Db)?;
                        account
                            .info
                            .code
                            .as_ref()
                            .and_then(Bytecode::eip7702_address)
                    };
                    if delegate.is_some_and(|address| {
                        crate::precompile_routes::resolve(&address).is_some()
                    }) {
                        // Follow exactly one designator. Revm still owns value
                        // transfer, checkpoints, gas and the empty-code result.
                        let empty = Bytecode::default();
                        inputs.known_bytecode = (empty.hash_slow(), empty);
                    }
                }
            }
        }
        self.0.frame_init(init)
    }
    fn frame_run(
        &mut self,
    ) -> Result<FrameInitOrResult<Self::Frame>, ContextDbError<Self::Context>> {
        self.0.frame_run()
    }
    fn frame_return_result(
        &mut self,
        result: FrameResult,
    ) -> Result<Option<FrameResult>, ContextDbError<Self::Context>> {
        self.0.frame_return_result(result)
    }
}

// Keep the default inspector frame methods: they call this adapter after
// inspector callbacks, whereas forwarding them would bypass normalization.
impl<E> revm::inspector::InspectorEvmTr for NativeDelegationEvm<&mut E>
where
    E: revm::inspector::InspectorEvmTr<Frame = EthFrame>,
{
    type Inspector = E::Inspector;
    fn all_inspector(
        &self,
    ) -> (
        &Self::Context,
        &Self::Instructions,
        &Self::Precompiles,
        &FrameStack<Self::Frame>,
        &Self::Inspector,
    ) {
        self.0.all_inspector()
    }
    fn all_mut_inspector(
        &mut self,
    ) -> (
        &mut Self::Context,
        &mut Self::Instructions,
        &mut Self::Precompiles,
        &mut FrameStack<Self::Frame>,
        &mut Self::Inspector,
    ) {
        self.0.all_mut_inspector()
    }
}
