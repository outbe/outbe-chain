//! The pause before the next delivery attempt of a message.
//!
//! Ported from hyperlane-monorepo `agents-v3.0.0`,
//! `rust/main/agents/relayer/src/msg/pending_message.rs`
//! (`calculate_msg_backoff`, Apache-2.0, see `LICENSE-APACHE`). Changes: the
//! spread of the late attempts comes from the message id, not from a random
//! number, and after the last attempt the caller drops the message.

use std::time::Duration;

use alloy_primitives::B256;

/// Attempts before the relayer drops a message (about two weeks).
pub const MAX_RETRIES: u32 = 66;
/// The first retries that wait only for validator signatures are fast: the
/// validators sign a new leaf within seconds.
const SIGNATURE_FAST_RETRIES: u32 = 3;
const SIGNATURE_FAST_PAUSE: u64 = 2;

/// The pause after failed attempt number `attempts` (from 1). `None`: the
/// message had its last attempt.
pub fn backoff(attempts: u32, awaiting_signatures: bool, message_id: B256) -> Option<Duration> {
    let attempts = if awaiting_signatures {
        if attempts <= SIGNATURE_FAST_RETRIES {
            return Some(Duration::from_secs(SIGNATURE_FAST_PAUSE));
        }
        // After the fast retries the normal schedule starts from its first step.
        attempts - SIGNATURE_FAST_RETRIES
    } else {
        attempts
    };
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    let secs = match attempts {
        0 | 1 => 5,
        2 => 10,
        3 => 30,
        4 => MINUTE,
        5..25 => 3 * MINUTE,
        // From 5 to 26 minutes, 1.5 minutes more for each attempt.
        25..40 => 5 * MINUTE + u64::from(attempts - 25) * 90,
        40..45 => 30 * MINUTE,
        45..50 => HOUR,
        // 2 hours more for each attempt, at a point of the next 6 hours that
        // depends on the message: old messages do not all return at one time.
        50..MAX_RETRIES => {
            let spread = u64::from_be_bytes(message_id[..8].try_into().expect("8 bytes"))
                .wrapping_add(u64::from(attempts));
            u64::from(attempts - 49) * 2 * HOUR + spread % (6 * HOUR)
        }
        _ => return None,
    };
    Some(Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_follows_upstream() {
        let id = B256::repeat_byte(7);
        let secs = |attempts| backoff(attempts, false, id).map(|d| d.as_secs());
        assert_eq!(
            [1, 2, 3, 4, 5, 24].map(secs),
            [5, 10, 30, 60, 180, 180].map(Some)
        );
        assert_eq!(
            [25, 39, 40, 44, 45, 49].map(secs),
            [300, 1560, 1800, 1800, 3600, 3600].map(Some)
        );
        let late = secs(50).unwrap();
        assert!((7_200..7_200 + 21_600).contains(&late), "{late}");
        let last = secs(65).unwrap();
        assert!((16 * 7_200..16 * 7_200 + 21_600).contains(&last), "{last}");
        assert_eq!(secs(66), None);

        // Missing signatures: three fast retries, then the normal schedule.
        let waiting = |attempts| backoff(attempts, true, id).map(|d| d.as_secs());
        assert_eq!(
            [1, 2, 3, 4, 5, 7].map(waiting),
            [2, 2, 2, 5, 10, 60].map(Some)
        );
        assert_eq!(waiting(69), None);
    }
}
