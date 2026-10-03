use super::*;

pub(super) struct JournalReader<'a> {
    encoded: &'a [u8],
    offset: usize,
}

impl<'a> JournalReader<'a> {
    pub(super) const fn new(encoded: &'a [u8]) -> Self {
        Self { encoded, offset: 0 }
    }

    pub(super) fn take<const N: usize>(&mut self) -> Result<[u8; N], RetentionError> {
        let end = self
            .offset
            .checked_add(N)
            .ok_or(RetentionError::MalformedJournal("offset overflow"))?;
        let value = self
            .encoded
            .get(self.offset..end)
            .ok_or(RetentionError::MalformedJournal("truncated field"))?;
        self.offset = end;
        value
            .try_into()
            .map_err(|_| RetentionError::MalformedJournal("field length"))
    }

    pub(super) fn record_bytes(&mut self) -> Result<&'a [u8], RetentionError> {
        let length = usize::from(u16::from_be_bytes(self.take::<2>()?));
        if length == 0 || length > PIN_RECORD_MAX_BYTES {
            return Err(RetentionError::MalformedJournal(
                "pin record length is outside its bound",
            ));
        }
        let end = self
            .offset
            .checked_add(length)
            .ok_or(RetentionError::MalformedJournal(
                "pin record offset overflow",
            ))?;
        let bytes = self
            .encoded
            .get(self.offset..end)
            .ok_or(RetentionError::MalformedJournal("truncated pin record"))?;
        self.offset = end;
        Ok(bytes)
    }

    pub(super) fn finish(self) -> Result<(), RetentionError> {
        if self.offset != self.encoded.len() {
            return Err(RetentionError::MalformedJournal("trailing bytes"));
        }
        Ok(())
    }
}
