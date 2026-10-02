//! bundles obligations for the offline OCOMP audit.
use super::*;

pub(super) fn read_pinned_bundle(root: &Path, hash: B256) -> eyre::Result<PinnedProtocolBundle> {
    use std::io::Read;
    let limits = poc_schema_limits();
    let source = outbe_snapshot::fs::SourceRoot::open(root)?;
    let catalog =
        std::path::PathBuf::from("protocol-bundles-v1").join(format!("{}.ocb1", hex::encode(hash)));
    let (path, mut entry) = match source.open_entry(&catalog) {
        Ok(entry) => (catalog, entry),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let path = std::path::PathBuf::from("protocol-bundle-v1.ocb1");
            let entry = source.open_entry(&path)?;
            (path, entry)
        }
        Err(error) => return Err(error.into()),
    };
    let cap = limits
        .codec
        .max_body_bytes
        .checked_add(outbe_ocomp_protocol::codec::OCB1_HEADER_LEN)
        .ok_or_else(|| eyre::eyre!("bundle codec limit overflow"))?;
    ensure!(
        !entry.identity.is_directory,
        "protocol bundle is not a file"
    );
    ensure!(
        entry.identity.size <= u64::try_from(cap)?,
        "protocol bundle exceeds codec limit"
    );
    let mut bytes = Vec::new();
    (&mut entry.file)
        .take(entry.identity.size + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 == entry.identity.size,
        "protocol bundle changed size"
    );
    entry.verify_unchanged()?;
    source.reopen(&path, &entry.identity)?;
    source.verify_unchanged()?;
    Ok(PinnedProtocolBundle::decode(&bytes, hash, &limits)?)
}

pub(super) fn classify_nod_input_error(error: eyre::Report, job: B256) -> eyre::Report {
    if error.downcast_ref::<Incomplete>().is_some() {
        return error;
    }
    if missing_native_input(error.as_ref()) {
        return error.wrap_err(Incomplete(format!(
            "missing required NOD input for job {job}"
        )));
    }
    error.wrap_err(format!("NOD inputs for job {job}"))
}

/// Transparent native wrappers can omit the wrapped enum from Error::source().
/// Follow their typed edges so missing records remain distinct from corrupt data.
pub(super) fn missing_native_input(error: &(dyn std::error::Error + 'static)) -> bool {
    use outbe_ocomp::{
        admission_catalog::AdmissionCatalogError as Admission,
        export_binding::ExportBindingError as Binding,
        export_receipt::ExportReceiptError as Receipt,
        input_artifacts::InputArtifactError as Input,
        input_ref_catalog::InputRefCatalogError as Refs,
        lysis_plan_audit::ExactLysisPlanError as Plan,
        lysis_result_catalog::LysisResultCatalogError as ResultCatalog,
        nod_materialization::NodMaterializationBuildErrorV1 as Build,
    };
    if let Some(error) = error.downcast_ref::<std::io::Error>() {
        return error.kind() == std::io::ErrorKind::NotFound;
    }
    if let Some(error) = error.downcast_ref::<Binding>() {
        return match error {
            Binding::MissingBinding => true,
            Binding::Cas(error) => missing_native_input(error),
            Binding::InputCatalog(error) => missing_native_input(error),
            Binding::Io { source, .. } => missing_native_input(source),
            _ => false,
        };
    }
    if let Some(error) = error.downcast_ref::<Receipt>() {
        return match error {
            Receipt::MissingReceipt | Receipt::MissingPreparation => true,
            Receipt::Cas(error) => missing_native_input(error),
            Receipt::Io { source, .. } => missing_native_input(source),
            _ => false,
        };
    }
    if let Some(error) = error.downcast_ref::<Admission>() {
        return match error {
            Admission::MissingHeader | Admission::MissingAdmission { .. } => true,
            Admission::Cas(error) => missing_native_input(error),
            Admission::Io { source, .. } => missing_native_input(source),
            _ => false,
        };
    }
    if let Some(error) = error.downcast_ref::<Refs>() {
        return match error {
            Refs::MissingHeader | Refs::MissingReference { .. } => true,
            Refs::Cas(error) => missing_native_input(error),
            Refs::Io { source, .. } => missing_native_input(source),
            _ => false,
        };
    }
    if let Some(error) = error.downcast_ref::<Input>() {
        return match error {
            Input::Cas(error) => missing_native_input(error),
            Input::InputRefCatalog(error) => missing_native_input(error),
            _ => false,
        };
    }
    if let Some(error) = error.downcast_ref::<Plan>() {
        return match error {
            Plan::Admission(error) => missing_native_input(error),
            Plan::InputRef(error) => missing_native_input(error),
            Plan::InputArtifact(error) => missing_native_input(error),
            Plan::Cas(error) => missing_native_input(error),
            _ => false,
        };
    }
    if let Some(error) = error.downcast_ref::<ResultCatalog>() {
        return match error {
            ResultCatalog::Plan(error) => missing_native_input(error),
            ResultCatalog::Admission(error) => missing_native_input(error),
            ResultCatalog::Cas(error) => missing_native_input(error),
            _ => false,
        };
    }
    if let Some(error) = error.downcast_ref::<Build>() {
        return match error {
            Build::Plan(error) => missing_native_input(error),
            Build::ResultCatalog(error) => missing_native_input(error),
            _ => false,
        };
    }
    error.source().is_some_and(missing_native_input)
}
