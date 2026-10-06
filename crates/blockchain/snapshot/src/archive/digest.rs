//! Hash native bytes as they are read, without buffering an entire payload.

use sha2::{Digest, Sha256};
use std::io::{self, Read};

pub(crate) struct DigestReader<R> {
    inner: R,
    digest: Sha256,
}
impl<R> DigestReader<R> {
    pub(crate) fn new(inner: R) -> Self {
        Self {
            inner,
            digest: Sha256::new(),
        }
    }
    pub(crate) fn finish(self) -> String {
        hex::encode(self.digest.finalize())
    }
}
impl<R: Read> Read for DigestReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buffer)?;
        self.digest.update(&buffer[..n]);
        Ok(n)
    }
}

pub(super) fn stream_sha256(reader: impl Read) -> io::Result<String> {
    let mut reader = DigestReader::new(reader);
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
    }
    Ok(reader.finish())
}
