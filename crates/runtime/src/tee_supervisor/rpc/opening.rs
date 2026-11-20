use super::proof::invalid;
use super::*;
use outbe_tee::finalized_admission::*;

pub(super) fn decode_opening(
    opening: &Value,
    slots: &[B256],
) -> Result<(MptAccountProofV1, Vec<MptStorageProofV1>), RpcLifecycleError> {
    let registry_account = MptAccountProofV1 {
        nonce: number(opening, "nonce")?,
        balance: word(opening, "balance")?,
        code_hash: hash(opening, "codeHash")?,
        storage_root: hash(opening, "storageHash")?,
        nodes: node_array(opening, "accountProof")?,
    };
    let storage = opening
        .get("storageProof")
        .and_then(Value::as_array)
        .ok_or(RpcLifecycleError::Missing("storageProof"))?;
    let mut registry_storage = Vec::with_capacity(storage.len());
    for item in storage {
        registry_storage.push(MptStorageProofV1 {
            key: hash(item, "key")?,
            value: word(item, "value")?,
            nodes: node_array(item, "proof")?,
        });
    }
    if registry_storage.len() != slots.len()
        || slots
            .iter()
            .any(|key| registry_storage.iter().filter(|p| p.key == *key).count() != 1)
    {
        return Err(invalid(ProofFailure::InvalidOpening));
    }
    Ok((registry_account, registry_storage))
}
fn node_array(value: &Value, name: &'static str) -> Result<Vec<Vec<u8>>, RpcLifecycleError> {
    value
        .get(name)
        .and_then(Value::as_array)
        .ok_or(RpcLifecycleError::Missing(name))?
        .iter()
        .map(|v| {
            let b: Bytes = serde_json::from_value(v.clone())?;
            Ok(b.to_vec())
        })
        .collect()
}
