use alloy_primitives::B256;
use outbe_ocomp::{
    discovery_control::{DiscoveryAckRefV1, DiscoveryOfferRefV1, DISCOVERY_CONTROL_VERSION_V1},
    discovery_transport::{DiscoveryOfferClientV1, DiscoveryOfferServerV1},
};

fn offer() -> DiscoveryOfferRefV1 {
    DiscoveryOfferRefV1 {
        version: DISCOVERY_CONTROL_VERSION_V1,
        chain_id: 42,
        genesis_hash: B256::repeat_byte(1),
        observation_id: B256::repeat_byte(2),
        generation: 3,
        discovery_record_digest: B256::repeat_byte(4),
    }
}

fn acknowledgment(offered: &DiscoveryOfferRefV1) -> DiscoveryAckRefV1 {
    DiscoveryAckRefV1 {
        version: offered.version,
        chain_id: offered.chain_id,
        genesis_hash: offered.genesis_hash,
        observation_id: offered.observation_id,
        generation: offered.generation,
        discovery_record_digest: offered.discovery_record_digest,
        export_receipt_digest: B256::repeat_byte(5),
    }
}

#[tokio::test]
async fn loopback_channel_carries_only_exact_offer_and_ack_refs() {
    let mut server = DiscoveryOfferServerV1::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let mut client = DiscoveryOfferClientV1::connect(server.address())
        .await
        .unwrap();
    let offered = offer();
    client.send_offer(&offered).await.unwrap();
    let received = server.receive_offer().await.unwrap();
    assert_eq!(received.reference(), &offered);

    let acknowledged = acknowledgment(&offered);
    server.send_ack(&received, &acknowledged).await.unwrap();
    assert_eq!(client.receive_ack(&offered).await.unwrap(), acknowledged);
}

#[tokio::test]
async fn lost_ack_is_recovered_by_exact_redelivery_without_transport_authority() {
    let mut server = DiscoveryOfferServerV1::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let offered = offer();

    let mut first = DiscoveryOfferClientV1::connect(server.address())
        .await
        .unwrap();
    first.send_offer(&offered).await.unwrap();
    let first_delivery = server.receive_offer().await.unwrap();
    assert_eq!(first_delivery.reference(), &offered);
    drop(first);

    let mut restarted = DiscoveryOfferClientV1::connect(server.address())
        .await
        .unwrap();
    restarted.send_offer(&offered).await.unwrap();
    let redelivery = server.receive_offer().await.unwrap();
    assert_eq!(redelivery.reference(), &offered);
    let acknowledged = acknowledgment(&offered);
    server.send_ack(&redelivery, &acknowledged).await.unwrap();
    assert_eq!(restarted.receive_ack(&offered).await.unwrap(), acknowledged);
}

#[tokio::test]
async fn non_loopback_and_conflicting_ack_are_rejected() {
    assert!(DiscoveryOfferServerV1::bind("0.0.0.0:0".parse().unwrap())
        .await
        .is_err());
    assert!(
        DiscoveryOfferClientV1::connect("192.0.2.1:30402".parse().unwrap())
            .await
            .is_err()
    );

    let mut server = DiscoveryOfferServerV1::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let mut client = DiscoveryOfferClientV1::connect(server.address())
        .await
        .unwrap();
    let offered = offer();
    client.send_offer(&offered).await.unwrap();
    let received = server.receive_offer().await.unwrap();
    let mut conflicting = acknowledgment(&offered);
    conflicting.generation += 1;
    assert!(server.send_ack(&received, &conflicting).await.is_err());
}
