use std::{
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};

use alloy_primitives::address;

use super::*;

struct FakeTransport {
    hyperlane: Address,
    funded: AtomicBool,
    signer_set: AtomicBool,
    funding_sends: AtomicUsize,
    send_delay: Duration,
}

impl HyperlaneSetupTransport for FakeTransport {
    async fn validator_delegate(&self, _validator: Address) -> Result<Address> {
        Ok(self.hyperlane)
    }

    async fn balance(&self, _address: Address) -> Result<U256> {
        if self.funded.load(Ordering::SeqCst) {
            Ok(U256::from(1_000_u64))
        } else {
            Ok(U256::ZERO)
        }
    }

    async fn validator(&self, _validator: Address) -> Result<ValidatorRecord> {
        Ok(ValidatorRecord {
            registered: true,
            status: 2,
            has_bls_share: true,
        })
    }

    async fn hyperlane_signer(&self, _validator: Address) -> Result<Address> {
        if self.signer_set.load(Ordering::SeqCst) {
            Ok(self.hyperlane)
        } else {
            Ok(Address::ZERO)
        }
    }

    async fn send_delegate(&self, _hyperlane: Address) -> Result<String> {
        unreachable!("delegate is already set in fixture")
    }

    async fn send_funding(&self, _hyperlane: Address, _funding: U256) -> Result<String> {
        self.funding_sends.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(self.send_delay).await;
        Ok("0x1111111111111111111111111111111111111111111111111111111111111111".into())
    }

    async fn send_set_signer(&self, _hyperlane: Address) -> Result<String> {
        self.signer_set.store(true, Ordering::SeqCst);
        Ok("0x2222222222222222222222222222222222222222222222222222222222222222".into())
    }

    async fn wait_finalized(
        &self,
        hash: &str,
        deadline: Option<Instant>,
        _progress: &mut Progress,
    ) -> Result<bool> {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Ok(false);
        }
        if hash.starts_with("0x1111") {
            self.funded.store(true, Ordering::SeqCst);
        }
        Ok(true)
    }
}

#[tokio::test]
async fn cooperative_deadline_journals_in_flight_send_and_retry_does_not_resend() {
    let validator = address!("1000000000000000000000000000000000000001");
    let hyperlane = address!("2000000000000000000000000000000000000002");
    let transport = FakeTransport {
        hyperlane,
        funded: AtomicBool::new(false),
        signer_set: AtomicBool::new(false),
        funding_sends: AtomicUsize::new(0),
        send_delay: Duration::from_millis(60),
    };
    let mut journaled: Option<SetupState> = None;
    let mut progress = Progress::new();

    // Deadline expires while `send_funding` is in flight: broadcast must finish and journal.
    let short_deadline = Some(Instant::now() + Duration::from_millis(20));
    let target = SetupTarget {
        validator,
        hyperlane,
        funding: U256::from(1_000_u64),
    };
    let (state, completed) = CooperativeSetup {
        transport: &transport,
        target,
        state: SetupState::default(),
        save: |next: &SetupState| {
            journaled = Some(next.clone());
            Ok(())
        },
        deadline: short_deadline,
        progress: &mut progress,
    }
    .run()
    .await
    .unwrap();
    assert!(!completed, "must report timeout after in-flight send");
    assert_eq!(transport.funding_sends.load(Ordering::SeqCst), 1);
    assert!(
        state.funding_transaction.is_some(),
        "in-flight funding hash must be returned"
    );
    assert_eq!(
        journaled
            .as_ref()
            .and_then(|s| s.funding_transaction.as_deref()),
        state.funding_transaction.as_deref(),
        "in-flight funding hash must be journaled via save"
    );

    // Retry with the journaled state must NOT send a second funding transaction.
    let generous_deadline = Some(Instant::now() + Duration::from_secs(5));
    let (final_state, completed) = CooperativeSetup {
        transport: &transport,
        target,
        state,
        save: |_: &SetupState| Ok(()),
        deadline: generous_deadline,
        progress: &mut progress,
    }
    .run()
    .await
    .unwrap();
    assert!(completed);
    assert_eq!(
        transport.funding_sends.load(Ordering::SeqCst),
        1,
        "retry must reuse the journaled funding hash without rebroadcasting"
    );
    assert!(final_state.signer_transaction.is_some());
}
