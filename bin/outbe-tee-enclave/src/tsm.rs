//! Linux TSM report interface (`configfs-tsm`).
//!
//! A confidential VM asks its platform for attestation evidence through one
//! kernel interface, `/sys/kernel/config/tsm/report`. The interface is the same
//! for AMD SEV-SNP and Intel TDX. The `provider` attribute names the platform.
//!
//! One request is:
//!
//! 1. create a directory for the request,
//! 2. write 64 bytes of report data to `inblob`,
//! 3. read the evidence from `outblob`,
//! 4. read the certificate table from `auxblob`, when the platform supplies one.
//!
//! The evidence is an SEV-SNP attestation report or a TDX quote. This module
//! does not interpret the evidence, except for the fixed-offset identity fields
//! of an SEV-SNP report that an operator needs to pin in the attestation policy.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::errors::{EnclaveResult, TsmError};

/// Default location of the TSM report interface.
pub const TSM_REPORT_DIR: &str = "/sys/kernel/config/tsm/report";

/// Length of the caller-supplied report data.
pub const REPORT_DATA_LEN: usize = 64;

/// Upper bound on the evidence and on the auxiliary data. A TDX quote is a few
/// kilobytes and an SEV-SNP report is 1184 bytes.
pub const MAX_BLOB_BYTES: usize = 64 * 1024;

/// Length of an SEV-SNP attestation report.
pub const SEV_SNP_REPORT_LEN: usize = 1184;

const SEV_SNP_MEASUREMENT_OFFSET: usize = 0x90;
const SEV_SNP_HOST_DATA_OFFSET: usize = 0xc0;

/// The dstack development simulator serves this interface only for request
/// names with this prefix. The kernel accepts any name.
const ENTRY_PREFIX: &str = "dstack-outbe";

/// Platform behind the TSM report interface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TsmProvider {
    /// AMD SEV-SNP guest (`sev_guest`).
    SevGuest,
    /// Intel TDX guest (`tdx_guest`).
    TdxGuest,
    /// A provider this service does not know.
    Other(String),
}

impl TsmProvider {
    fn parse(text: &str) -> Self {
        match text.trim() {
            "sev_guest" => Self::SevGuest,
            "tdx_guest" => Self::TdxGuest,
            other => Self::Other(other.to_owned()),
        }
    }

    /// Provider name as the kernel reports it.
    pub fn name(&self) -> &str {
        match self {
            Self::SevGuest => "sev_guest",
            Self::TdxGuest => "tdx_guest",
            Self::Other(name) => name,
        }
    }
}

/// Evidence returned for one request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TsmReport {
    pub provider: TsmProvider,
    /// SEV-SNP attestation report or TDX quote.
    pub report: Vec<u8>,
    /// Platform certificate table. Empty when the platform supplies none.
    pub auxiliary: Vec<u8>,
}

/// Launch identity of an SEV-SNP guest, as carried in its attestation report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SevSnpIdentity {
    /// Launch measurement of the VM image.
    pub measurement: [u8; 48],
    /// Host-supplied launch data. dstack binds the application manifest here.
    pub host_data: [u8; 32],
}

/// Read the launch identity from an SEV-SNP attestation report.
///
/// This reads fixed offsets and does not authenticate the report.
pub fn sev_snp_identity(report: &[u8]) -> EnclaveResult<SevSnpIdentity> {
    if report.len() != SEV_SNP_REPORT_LEN {
        return Err(TsmError::MalformedReport {
            platform: "SEV-SNP",
            detail: format!("length is {}, expected {SEV_SNP_REPORT_LEN}", report.len()),
        }
        .into());
    }
    let mut measurement = [0_u8; 48];
    measurement
        .copy_from_slice(&report[SEV_SNP_MEASUREMENT_OFFSET..SEV_SNP_MEASUREMENT_OFFSET + 48]);
    let mut host_data = [0_u8; 32];
    host_data.copy_from_slice(&report[SEV_SNP_HOST_DATA_OFFSET..SEV_SNP_HOST_DATA_OFFSET + 32]);
    Ok(SevSnpIdentity {
        measurement,
        host_data,
    })
}

/// Handle to a TSM report interface directory.
#[derive(Clone, Debug)]
pub struct TsmReportSource {
    root: PathBuf,
    /// Request directory to use instead of a fresh one. Tests prepare it.
    #[cfg(test)]
    fixed_entry: Option<String>,
}

impl TsmReportSource {
    /// The interface of the running kernel.
    pub fn system() -> Self {
        Self::at(TSM_REPORT_DIR)
    }

    /// An interface rooted at `root`.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            #[cfg(test)]
            fixed_entry: None,
        }
    }

    /// An interface whose every request uses the prepared directory `entry`.
    #[cfg(test)]
    pub(crate) fn with_prepared_entry(root: impl Into<PathBuf>, entry: &str) -> Self {
        Self {
            root: root.into(),
            fixed_entry: Some(entry.to_owned()),
        }
    }

    /// True when the interface directory exists.
    pub fn is_available(&self) -> bool {
        self.root.is_dir()
    }

    /// Request evidence that binds `report_data`.
    pub fn report(&self, report_data: &[u8; REPORT_DATA_LEN]) -> EnclaveResult<TsmReport> {
        #[cfg(test)]
        if let Some(entry) = &self.fixed_entry {
            return self.report_in_entry(entry, report_data);
        }
        static REQUEST: AtomicU64 = AtomicU64::new(0);
        let name = format!(
            "{ENTRY_PREFIX}-{}-{}",
            std::process::id(),
            REQUEST.fetch_add(1, Ordering::Relaxed)
        );
        self.report_in_entry(&name, report_data)
    }

    fn report_in_entry(
        &self,
        name: &str,
        report_data: &[u8; REPORT_DATA_LEN],
    ) -> EnclaveResult<TsmReport> {
        let entry = self.root.join(name);
        match fs::create_dir(&entry) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(TsmError::io("create", entry.display(), error).into()),
        }
        let result = self.read_entry(&entry, report_data);
        // The kernel frees the request state when its directory is removed.
        let _ = fs::remove_dir(&entry);
        result
    }

    fn read_entry(
        &self,
        entry: &Path,
        report_data: &[u8; REPORT_DATA_LEN],
    ) -> EnclaveResult<TsmReport> {
        let inblob = entry.join("inblob");
        // Open without truncation: a configfs attribute has no truncate operation.
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&inblob)
            .map_err(|error| TsmError::io("open", inblob.display(), error))?;
        file.write_all(report_data)
            .map_err(|error| TsmError::io("write", inblob.display(), error))?;
        drop(file);

        let provider = self.provider_of(entry)?;
        let outblob = entry.join("outblob");
        let report = read_bounded(&outblob, "report")?;
        if report.is_empty() {
            return Err(TsmError::EmptyReport {
                path: outblob.display().to_string(),
            }
            .into());
        }
        let auxiliary = match ["auxblob", "certs"]
            .iter()
            .map(|file| entry.join(file))
            .find(|path| path.exists())
        {
            Some(path) => read_bounded(&path, "auxiliary data")?,
            None => Vec::new(),
        };
        Ok(TsmReport {
            provider,
            report,
            auxiliary,
        })
    }

    /// Platform that serves this interface.
    ///
    /// The kernel exposes the `provider` attribute in every request directory
    /// and not in the interface directory. This function therefore creates a
    /// request directory, reads the attribute and removes the directory. The
    /// dstack simulator also has the attribute in the interface directory.
    pub fn provider(&self) -> EnclaveResult<TsmProvider> {
        if self.root.join("provider").exists() {
            return self.provider_of(&self.root);
        }
        #[cfg(test)]
        if let Some(entry) = &self.fixed_entry {
            return self.provider_of(&self.root.join(entry));
        }
        static REQUEST: AtomicU64 = AtomicU64::new(0);
        let entry = self.root.join(format!(
            "{ENTRY_PREFIX}-provider-{}-{}",
            std::process::id(),
            REQUEST.fetch_add(1, Ordering::Relaxed)
        ));
        match fs::create_dir(&entry) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(TsmError::io("create", entry.display(), error).into()),
        }
        let result = self.provider_of(&entry);
        let _ = fs::remove_dir(&entry);
        result
    }

    fn provider_of(&self, entry: &Path) -> EnclaveResult<TsmProvider> {
        let path = [entry.join("provider"), self.root.join("provider")]
            .into_iter()
            .find(|path| path.exists())
            .ok_or_else(|| TsmError::io("read", entry.join("provider").display(), "not found"))?;
        let text = fs::read_to_string(&path)
            .map_err(|error| TsmError::io("read", path.display(), error))?;
        Ok(TsmProvider::parse(&text))
    }
}

fn read_bounded(path: &Path, what: &'static str) -> EnclaveResult<Vec<u8>> {
    let bytes = fs::read(path).map_err(|error| TsmError::io("read", path.display(), error))?;
    if bytes.len() > MAX_BLOB_BYTES {
        return Err(TsmError::TooLarge {
            what,
            len: bytes.len(),
            max: MAX_BLOB_BYTES,
        }
        .into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::EnclaveError;

    const ENTRY: &str = "request";

    /// A directory that behaves like one prepared TSM report entry.
    fn fake_interface(
        provider: &str,
        report: &[u8],
        auxiliary: Option<&[u8]>,
    ) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let entry = root.path().join(ENTRY);
        fs::create_dir(&entry).unwrap();
        fs::write(entry.join("provider"), provider).unwrap();
        fs::write(entry.join("outblob"), report).unwrap();
        if let Some(auxiliary) = auxiliary {
            fs::write(entry.join("auxblob"), auxiliary).unwrap();
        }
        root
    }

    fn tsm_error(result: EnclaveResult<TsmReport>) -> TsmError {
        match result {
            Err(EnclaveError::Tsm(error)) => error,
            other => panic!("expected a TSM error, got {other:?}"),
        }
    }

    #[test]
    fn report_writes_report_data_and_returns_evidence_with_certificates() {
        let root = fake_interface("sev_guest\n", b"report-bytes", Some(b"certificate-table"));
        let report_data = [0x5a_u8; REPORT_DATA_LEN];

        let report = TsmReportSource::at(root.path())
            .report_in_entry(ENTRY, &report_data)
            .unwrap();

        assert_eq!(report.provider, TsmProvider::SevGuest);
        assert_eq!(report.report, b"report-bytes");
        assert_eq!(report.auxiliary, b"certificate-table");
        assert_eq!(
            fs::read(root.path().join(ENTRY).join("inblob")).unwrap(),
            report_data
        );
    }

    #[test]
    fn report_without_certificate_table_has_empty_auxiliary_data() {
        let root = fake_interface("tdx_guest", b"quote", None);

        let report = TsmReportSource::at(root.path())
            .report_in_entry(ENTRY, &[0; REPORT_DATA_LEN])
            .unwrap();

        assert_eq!(report.provider, TsmProvider::TdxGuest);
        assert!(report.auxiliary.is_empty());
    }

    #[test]
    fn certificate_table_is_read_from_the_alternative_attribute_name() {
        let root = fake_interface("sev_guest", b"report", None);
        fs::write(root.path().join(ENTRY).join("certs"), b"table").unwrap();

        let report = TsmReportSource::at(root.path())
            .report_in_entry(ENTRY, &[0; REPORT_DATA_LEN])
            .unwrap();

        assert_eq!(report.auxiliary, b"table");
    }

    #[test]
    fn provider_is_read_from_a_request_directory_of_a_real_kernel() {
        // The kernel has `provider` in the request directory only.
        let root = fake_interface("sev_guest\n", b"report", None);
        assert!(!root.path().join("provider").exists());
        let source = TsmReportSource::with_prepared_entry(root.path(), ENTRY);
        assert_eq!(source.provider().unwrap(), TsmProvider::SevGuest);
    }

    #[test]
    fn provider_lookup_creates_and_removes_its_request_directory() {
        // Without a `provider` attribute anywhere the lookup fails and leaves
        // no request directory behind.
        let root = tempfile::tempdir().unwrap();
        assert!(TsmReportSource::at(root.path()).provider().is_err());
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn provider_falls_back_to_the_interface_root() {
        let root = fake_interface("ignored", b"report", None);
        fs::remove_file(root.path().join(ENTRY).join("provider")).unwrap();
        fs::write(root.path().join("provider"), "sev_guest\n").unwrap();
        let source = TsmReportSource::at(root.path());

        assert_eq!(source.provider().unwrap(), TsmProvider::SevGuest);
        let report = source
            .report_in_entry(ENTRY, &[0; REPORT_DATA_LEN])
            .unwrap();
        assert_eq!(report.provider, TsmProvider::SevGuest);
    }

    #[test]
    fn unknown_provider_keeps_its_name() {
        let root = fake_interface("arm_cca_guest\n", b"report", None);

        let report = TsmReportSource::at(root.path())
            .report_in_entry(ENTRY, &[0; REPORT_DATA_LEN])
            .unwrap();

        assert_eq!(report.provider, TsmProvider::Other("arm_cca_guest".into()));
        assert_eq!(report.provider.name(), "arm_cca_guest");
    }

    #[test]
    fn empty_report_is_rejected() {
        let root = fake_interface("sev_guest", b"", None);

        let error = tsm_error(
            TsmReportSource::at(root.path()).report_in_entry(ENTRY, &[0; REPORT_DATA_LEN]),
        );

        assert!(matches!(error, TsmError::EmptyReport { .. }));
    }

    #[test]
    fn oversized_report_is_rejected() {
        let root = fake_interface("sev_guest", &vec![1_u8; MAX_BLOB_BYTES + 1], None);

        let error = tsm_error(
            TsmReportSource::at(root.path()).report_in_entry(ENTRY, &[0; REPORT_DATA_LEN]),
        );

        assert_eq!(
            error,
            TsmError::TooLarge {
                what: "report",
                len: MAX_BLOB_BYTES + 1,
                max: MAX_BLOB_BYTES
            }
        );
    }

    #[test]
    fn missing_interface_is_not_available_and_fails_the_request() {
        let root = tempfile::tempdir().unwrap();
        let source = TsmReportSource::at(root.path().join("absent"));

        assert!(!source.is_available());
        let error = tsm_error(source.report(&[0; REPORT_DATA_LEN]));
        assert!(matches!(
            error,
            TsmError::Io {
                operation: "create",
                ..
            }
        ));
    }

    #[test]
    fn each_request_uses_its_own_entry() {
        let root = tempfile::tempdir().unwrap();
        let source = TsmReportSource::at(root.path());

        // A plain directory has no `outblob`, so both requests fail at the read.
        // The entries are removed again, and the names differ between requests.
        let first = tsm_error(source.report(&[0; REPORT_DATA_LEN]));
        let second = tsm_error(source.report(&[0; REPORT_DATA_LEN]));

        let path = |error: &TsmError| match error {
            TsmError::Io { path, .. } => path.clone(),
            other => panic!("unexpected error {other:?}"),
        };
        assert_ne!(path(&first), path(&second));
    }

    #[test]
    fn sev_snp_identity_reads_measurement_and_host_data() {
        let mut report = vec![0_u8; SEV_SNP_REPORT_LEN];
        report[0x90..0xc0].copy_from_slice(&[0xaa; 48]);
        report[0xc0..0xe0].copy_from_slice(&[0xbb; 32]);
        report[0x8f] = 0x11;
        report[0xe0] = 0x22;

        let identity = sev_snp_identity(&report).unwrap();

        assert_eq!(identity.measurement, [0xaa; 48]);
        assert_eq!(identity.host_data, [0xbb; 32]);
    }

    #[test]
    fn sev_snp_identity_rejects_a_report_of_another_length() {
        let result = sev_snp_identity(&vec![0_u8; SEV_SNP_REPORT_LEN - 1]);

        assert!(matches!(
            result,
            Err(EnclaveError::Tsm(TsmError::MalformedReport {
                platform: "SEV-SNP",
                ..
            }))
        ));
    }
}
