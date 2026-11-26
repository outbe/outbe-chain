use outbe_macros::{contract, storage_schema};
use outbe_system_common::addresses::METADOSIS_ADDRESS;

/// Native Metadosis event-emitting contract handle at `METADOSIS_ADDRESS`.
///
/// Metadosis reads the `EmissionDay` record with [`outbe_emission::api::get_emission_day`].
#[storage_schema]
#[contract(addr = METADOSIS_ADDRESS)]
pub struct MetadosisContract {}
