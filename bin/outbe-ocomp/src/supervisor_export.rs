//! Supervisor-side adoption of an exporter receipt into computation authority.

use std::path::PathBuf;

use outbe_ocomp_protocol::SchemaLimits;
use thiserror::Error;

use crate::{
    bundle::PinnedProtocolBundle,
    cas::{CasLimits, CasWriterRole, FilesystemCas, FilesystemCasReader},
    export_binding::{
        ExportReceiptBindingCandidate, ExportedManifestBindingStore,
        VerifiedExportedManifestBinding,
    },
    export_receipt::ExportReceiptReader,
    input_artifacts::poc_input_list_limits,
    input_ref_catalog::VerifiedInputChunkRefCatalog,
    supervisor::DiscoveryRecord,
};

#[derive(Clone, Debug)]
pub struct SupervisorExportAdoptionConfig {
    pub cas_root: PathBuf,
    pub cas_limits: CasLimits,
    pub input_ref_root: PathBuf,
    pub receipt_root: PathBuf,
    pub binding_root: PathBuf,
    pub protocol_bundle: PinnedProtocolBundle,
    pub limits: SchemaLimits,
}

pub enum SupervisorExportAdoptionOutcome {
    Pending,
    Adopted(Box<VerifiedExportedManifestBinding>),
}

pub struct SupervisorExportAdoption {
    config: SupervisorExportAdoptionConfig,
    cas: FilesystemCas,
    reader: FilesystemCasReader,
}

impl SupervisorExportAdoption {
    pub fn open(
        config: SupervisorExportAdoptionConfig,
    ) -> Result<Self, SupervisorExportAdoptionError> {
        let cas = stage(
            "open supervisor CAS writer",
            FilesystemCas::open(
                &config.cas_root,
                CasWriterRole::Supervisor,
                config.cas_limits,
            ),
        )?;
        let reader = stage(
            "open supervisor CAS reader",
            FilesystemCasReader::open(&config.cas_root, config.cas_limits),
        )?;
        Ok(Self {
            config,
            cas,
            reader,
        })
    }

    pub fn try_adopt(
        &self,
        discovery: &DiscoveryRecord,
    ) -> Result<SupervisorExportAdoptionOutcome, SupervisorExportAdoptionError> {
        let job_id = discovery.spec.summary.job_id;
        let Some(receipt_reader) = stage(
            "inspect exporter receipt",
            ExportReceiptReader::try_open(&self.config.receipt_root, job_id, self.config.limits),
        )?
        else {
            return Ok(SupervisorExportAdoptionOutcome::Pending);
        };
        let receipt = stage(
            "load exact exporter receipt",
            receipt_reader.load_exact(&self.reader),
        )?;
        let input_refs = stage(
            "reopen exported input-ref catalog read-only",
            VerifiedInputChunkRefCatalog::reopen(
                self.config
                    .input_ref_root
                    .join(hex::encode(job_id.as_slice())),
                &self.reader,
                self.config.limits,
                poc_input_list_limits(),
            ),
        )?;
        let mut binding_store = stage(
            "open supervisor export-binding journal",
            ExportedManifestBindingStore::open(
                self.config
                    .binding_root
                    .join(hex::encode(job_id.as_slice())),
                self.config.limits,
            ),
        )?;
        let (_, binding) = stage(
            "seal verified exported-manifest binding",
            binding_store.seal_receipt(
                &self.cas,
                &self.reader,
                ExportReceiptBindingCandidate {
                    discovery,
                    receipt: &receipt,
                    bundle: self.config.protocol_bundle.bundle(),
                    input_refs: &input_refs,
                },
            ),
        )?;
        Ok(SupervisorExportAdoptionOutcome::Adopted(Box::new(binding)))
    }
}

fn stage<T, E: std::fmt::Display>(
    stage: &'static str,
    result: Result<T, E>,
) -> Result<T, SupervisorExportAdoptionError> {
    result.map_err(|error| SupervisorExportAdoptionError::Stage {
        stage,
        detail: error.to_string(),
    })
}

#[derive(Debug, Error)]
pub enum SupervisorExportAdoptionError {
    #[error("supervisor export-adoption stage `{stage}` failed: {detail}")]
    Stage { stage: &'static str, detail: String },
}
