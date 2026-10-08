use super::*;
use commonware_codec::{DecodeExt as _, Encode as _};

#[test]
fn opaque_key_preserves_height_binding_and_codec() {
    let key = Key::Block(Digest::ZERO);
    let a = BoundKey::new(
        key,
        &Annotation::Certified {
            height: Height::new(7),
        },
    );
    let b = BoundKey::new(
        key,
        &Annotation::Finalized(handler::Finalized::ByHeight {
            height: Height::new(7),
        }),
    );
    let other = BoundKey::new(
        key,
        &Annotation::Certified {
            height: Height::new(8),
        },
    );
    assert_eq!(a, b);
    assert_ne!(a, other);
    assert_eq!(BoundKey::decode(a.encode()).unwrap(), a);
}

#[test]
fn source_backoff_caps_and_canceled_state_expires() {
    let key = BoundKey {
        key: Key::Finalized {
            height: Height::new(7),
        },
        height: None,
    };
    let mut retries = RetryState::default();
    let now = SystemTime::UNIX_EPOCH;
    let mut delay = retries.begin(key, now);
    assert_eq!(delay, Duration::ZERO);
    for _ in 0..12 {
        retries.failed(key, delay, now);
        delay = retries.begin(key, now);
    }
    assert_eq!(delay, MAX_RETRY_DELAY);
    assert_eq!(retries.begin(key, now + RETRY_STATE_TTL), Duration::ZERO);
}
