use outbe_primitives::system_tx::{end_block_nonce_ordinal, SystemTx};

pub(crate) fn end_block_inputs() -> [(SystemTx, u8); 5] {
    [
        (
            SystemTx::EndBlock { step_index: 0 },
            end_block_nonce_ordinal(0).unwrap(),
        ),
        (
            SystemTx::EndBlock { step_index: 1 },
            end_block_nonce_ordinal(1).unwrap(),
        ),
        (
            SystemTx::EndBlock { step_index: 2 },
            end_block_nonce_ordinal(2).unwrap(),
        ),
        (
            SystemTx::EndBlock { step_index: 3 },
            end_block_nonce_ordinal(3).unwrap(),
        ),
        (
            SystemTx::EndBlock { step_index: 4 },
            end_block_nonce_ordinal(4).unwrap(),
        ),
    ]
}
