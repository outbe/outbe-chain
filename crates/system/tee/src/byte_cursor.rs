//! Bounded forward reader over one byte slice.
//!
//! The canonical codecs of this crate read fixed and length-prefixed fields
//! with the same bounds rules. This reader owns those rules. Each codec gives
//! the function that maps a read failure to its own error.

/// The cause of a failed bounded read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CursorError {
    /// The read length makes the end offset overflow `usize`.
    Overflow,
    /// The read ends after the end of the input.
    Truncated,
    /// Bytes remain after the last read.
    Trailing,
}

/// Reads fields in order from `bytes`. The offset only increases.
pub(crate) struct ByteCursor<'a, E> {
    bytes: &'a [u8],
    offset: usize,
    error: fn(CursorError) -> E,
}

impl<'a, E> ByteCursor<'a, E> {
    pub(crate) const fn new(bytes: &'a [u8], error: fn(CursorError) -> E) -> Self {
        Self {
            bytes,
            offset: 0,
            error,
        }
    }

    /// Returns the next `len` bytes. The offset first checks for overflow and
    /// then for the end of the input. A failed read does not move the offset.
    pub(crate) fn take(&mut self, len: usize) -> Result<&'a [u8], E> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or_else(|| (self.error)(CursorError::Overflow))?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| (self.error)(CursorError::Truncated))?;
        self.offset = end;
        Ok(value)
    }

    pub(crate) fn array<const N: usize>(&mut self) -> Result<[u8; N], E> {
        let mut value = [0; N];
        value.copy_from_slice(self.take(N)?);
        Ok(value)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, E> {
        let [value] = self.array()?;
        Ok(value)
    }

    pub(crate) fn u16_be(&mut self) -> Result<u16, E> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    pub(crate) fn u32_be(&mut self) -> Result<u32, E> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    pub(crate) fn u64_be(&mut self) -> Result<u64, E> {
        Ok(u64::from_be_bytes(self.array()?))
    }

    /// Accepts the input only if every byte was read.
    pub(crate) fn finish(self) -> Result<(), E> {
        if self.offset != self.bytes.len() {
            return Err((self.error)(CursorError::Trailing));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursor(bytes: &[u8]) -> ByteCursor<'_, CursorError> {
        ByteCursor::new(bytes, |error| error)
    }

    #[test]
    fn reads_fields_in_order_and_requires_full_consumption() {
        let mut reader = cursor(&[1, 0, 2, 0, 0, 0, 3, 9, 9]);
        assert_eq!(reader.u8(), Ok(1));
        assert_eq!(reader.u16_be(), Ok(2));
        assert_eq!(reader.u32_be(), Ok(3));
        assert_eq!(reader.take(1), Ok(&[9][..]));
        assert_eq!(reader.finish(), Err(CursorError::Trailing));

        let mut reader = cursor(&[0, 0, 0, 0, 0, 0, 0, 7]);
        assert_eq!(reader.u64_be(), Ok(7));
        assert_eq!(reader.finish(), Ok(()));
    }

    #[test]
    fn overflow_is_checked_before_truncation_and_keeps_the_offset() {
        let mut reader = cursor(&[5, 6]);
        assert_eq!(reader.take(1), Ok(&[5][..]));
        assert_eq!(reader.take(usize::MAX), Err(CursorError::Overflow));
        assert_eq!(reader.take(2), Err(CursorError::Truncated));
        assert_eq!(reader.array::<1>(), Ok([6]));
        assert_eq!(reader.finish(), Ok(()));
    }
}
