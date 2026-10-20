use alloy_primitives::B256;
use outbe_primitives::{
    error::Result,
    storage::{MetadosisMutationPurpose, StorageHandle},
};
use outbe_tribute::worldwideday::ValidatedWwdAggregate;

/// The single production mutation seam. The provider grants an exact
/// purpose-bound lease. Its checkpoint covers state and EVM events.
pub(crate) fn commit_transition<P, R>(
    storage: StorageHandle<'_>,
    binding: B256,
    effect: impl FnOnce(StorageHandle<'_>) -> Result<R>,
) -> Result<R>
where
    P: MetadosisMutationPurpose,
{
    let command_storage = storage.clone();
    storage.with_metadosis_mutation::<P, R>(binding, || {
        ValidatedWwdAggregate::load_and_validate(command_storage.clone())?;
        let result = effect(command_storage.clone())?;
        ValidatedWwdAggregate::load_and_validate(command_storage)?;
        Ok(result)
    })
}
