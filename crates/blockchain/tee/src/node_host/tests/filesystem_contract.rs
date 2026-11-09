use super::*;

struct RecordFixture {
    directory: tempfile::TempDir,
    path: PathBuf,
    scratch: PathBuf,
}

impl RecordFixture {
    fn new() -> std::io::Result<Self> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("record");
        let scratch = directory.path().join("scratch");
        Ok(Self {
            directory,
            path,
            scratch,
        })
    }

    fn write(&self, bytes: &[u8]) -> Result<(), TransportError> {
        write_bytes_once_or_exact(
            &self.path,
            &self.scratch,
            BoundedRecordBytes {
                bytes,
                maximum_len: 16,
                label: "fixture",
            },
            self.directory.path(),
        )
    }
}

#[test]
fn owned_record_rejects_public_permissions_and_oversized_contents(
) -> Result<(), Box<dyn std::error::Error>> {
    let record = RecordFixture::new()?;
    record.write(b"abc")?;
    assert_eq!(
        read_owned_bounded_file(&record.path, 16, "fixture")?,
        b"abc"
    );
    fs::set_permissions(&record.path, fs::Permissions::from_mode(0o644))?;
    assert!(
        matches!(read_owned_bounded_file(&record.path, 16, "fixture"),
        Err(TransportError::Codec(message)) if message == "NodeHost fixture must be an owner-only bounded regular file")
    );
    fs::set_permissions(&record.path, fs::Permissions::from_mode(0o600))?;
    fs::write(&record.path, [0_u8; 17])?;
    assert!(
        matches!(read_owned_bounded_file(&record.path, 16, "fixture"),
        Err(TransportError::Codec(message)) if message == "NodeHost fixture must be an owner-only bounded regular file")
    );
    Ok(())
}

#[test]
fn read_limit_overflow_is_reported_before_reading() -> Result<(), Box<dyn std::error::Error>> {
    let record = RecordFixture::new()?;
    record.write(b"abc")?;
    assert!(
        matches!(read_owned_bounded_file(&record.path, u64::MAX, "fixture"),
        Err(TransportError::Codec(message)) if message == "NodeHost fixture read limit overflow")
    );
    Ok(())
}

#[test]
fn identical_record_retry_keeps_existing_record_and_scratch(
) -> Result<(), Box<dyn std::error::Error>> {
    let record = RecordFixture::new()?;
    record.write(b"first")?;
    fs::write(&record.scratch, b"sentinel")?;
    record.write(b"first")?;
    assert_eq!(fs::read(&record.path)?, b"first");
    assert_eq!(fs::read(&record.scratch)?, b"sentinel");
    Ok(())
}

#[test]
fn conflicting_record_keeps_existing_record_and_scratch() -> Result<(), Box<dyn std::error::Error>>
{
    let record = RecordFixture::new()?;
    record.write(b"first")?;
    fs::write(&record.scratch, b"sentinel")?;
    assert!(
        matches!(record.write(b"second"), Err(TransportError::Codec(message))
        if message == "durable NodeHost fixture conflicts with the requested value")
    );
    assert_eq!(fs::read(&record.path)?, b"first");
    assert_eq!(fs::read(&record.scratch)?, b"sentinel");
    Ok(())
}

/// `path_exists` keeps the `TransportError::Io` mapping of IO errors.
#[test]
fn path_exists_maps_io_errors_to_transport_io() -> std::io::Result<()> {
    let directory = tempfile::tempdir()?;
    let file = directory.path().join("file");
    fs::write(&file, b"x")?;
    assert!(matches!(
        path_exists(&file.join("child")),
        Err(crate::TransportError::Io(_))
    ));
    Ok(())
}
