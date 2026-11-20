use super::proof::invalid;
use super::*;

impl RpcLifecyclePort {
    pub(super) async fn receipt(
        &self,
        at: LifecycleCheckpointV1,
        tx_hash: B256,
    ) -> Result<Option<LifecycleReceiptV1>, RpcLifecycleError> {
        self.check(at).await?;
        let receipt: Option<Value> = self
            .client
            .request("eth_getTransactionReceipt", rpc_params![tx_hash])
            .await?;
        let Some(receipt) = receipt else {
            return Ok(None);
        };
        let height = number(&receipt, "blockNumber")?;
        if height > at.height {
            return Ok(None);
        }
        let receipt = self.confirmed_receipt(&receipt, tx_hash, height).await?;
        self.check(at).await?;
        Ok(Some(receipt))
    }
    pub(super) async fn canonical_block(
        &self,
        height: u64,
        expected_hash: B256,
    ) -> Result<Value, RpcLifecycleError> {
        let block = self.block(json!(format!("0x{height:x}"))).await?;
        if number(&block, "number")? != height || hash(&block, "hash")? != expected_hash {
            return Err(RpcLifecycleError::CheckpointMismatch);
        }
        Ok(block)
    }
    async fn confirmed_receipt(
        &self,
        receipt: &Value,
        tx_hash: B256,
        height: u64,
    ) -> Result<LifecycleReceiptV1, RpcLifecycleError> {
        if hash(receipt, "transactionHash")? != tx_hash {
            return Err(invalid(ProofFailure::InvalidReceipt));
        }
        let block_hash = hash(receipt, "blockHash")?;
        self.canonical_block(height, block_hash).await?;
        let status = number(receipt, "status")?;
        if status > 1 {
            return Err(invalid(ProofFailure::InvalidReceipt));
        }
        Ok(LifecycleReceiptV1 {
            tx_hash,
            block_height: height,
            block_hash,
            success: status == 1,
        })
    }
}
