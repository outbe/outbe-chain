use std::fmt::Debug;

use outbe_ocomp_protocol::ProtocolError;

pub(super) fn assert_strict_canonical_record<T>(
    value: T,
    encode: impl FnOnce(&T) -> Vec<u8>,
    decode: impl Fn(&[u8]) -> Result<T, ProtocolError>,
) where
    T: Debug + PartialEq,
{
    let mut encoded = encode(&value);
    assert_eq!(decode(&encoded).unwrap(), value);
    encoded.push(0);
    assert!(matches!(
        decode(&encoded),
        Err(ProtocolError::TrailingBytes { .. })
    ));
}
