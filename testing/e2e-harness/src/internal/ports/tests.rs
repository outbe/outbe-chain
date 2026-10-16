use std::collections::HashSet;
use std::net::TcpListener;
use std::path::Path;

use super::Service::*;
use super::*;

/// One scenario's worth of the static layout: no probing, block `i` at
/// `NODE_BASE + i * BLOCK`.
fn static_ports(n: usize) -> Ports {
    let p = Ports::new(false);
    p.start_scenario(n)
        .expect("static layout cannot exhaust the port space");
    p
}

#[test]
fn unscanned_layout_is_block_per_node() {
    let p = static_ports(3);
    assert_eq!(p.port(Http, 0), 18545);
    assert_eq!(p.port(Tee, 0), 18546);
    assert_eq!(p.port(Consensus, 0), 18551);
    assert_eq!(p.port(OcompSupervisor, 0), 18552);
    assert_eq!(p.port(OcompSnapshotExporter, 0), 18564);
    assert_eq!(p.port(Radicle, 0), RADICLE_BASE);
    assert_eq!(p.port(RadicleStatus, 0), RADICLE_BASE + 1);
    assert_eq!(p.port(Http, 1), NODE_BASE + BLOCK);
    assert_eq!(p.port(Tee, 1), NODE_BASE + BLOCK + 1);
    assert_eq!(p.port(Radicle, 1), RADICLE_BASE + RADICLE_BLOCK);
}

#[test]
fn ocomp_derived_listeners_are_inside_each_disjoint_node_block() {
    let p = static_ports(4);
    for index in 0..4 {
        let supervisor = p.port(OcompSupervisor, index);
        assert_eq!(p.port(OcompMessage, index), supervisor + 1);
        assert_eq!(p.port(OcompWorker0, index), supervisor + 2);
        assert_eq!(p.port(OcompWorker3, index), supervisor + 5);
        assert_eq!(p.port(OcompSuccessorSupervisor, index), supervisor + 6);
        assert_eq!(p.port(OcompSuccessorMessage, index), supervisor + 7);
        assert_eq!(p.port(OcompSuccessorWorker0, index), supervisor + 8);
        assert_eq!(p.port(OcompSuccessorWorker3, index), supervisor + 11);
        assert_eq!(p.port(OcompSnapshotExporter, index), supervisor + 12);
    }
}

#[test]
fn canonical_ocomp_fixture_uses_the_current_static_consensus_ports() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/ocomp-final-v1/base/validators.json");
    let validators: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture).expect("canonical validators fixture"))
            .expect("canonical validators JSON");
    let validators = validators.as_array().expect("validators array");
    let ports = static_ports(validators.len());

    for (index, validator) in validators.iter().enumerate() {
        let address = validator["p2p_address"]
            .as_str()
            .expect("validator p2p_address");
        let fixture_port = address
            .rsplit_once(':')
            .and_then(|(_, port)| port.parse::<u16>().ok())
            .expect("validator p2p port");
        assert_eq!(
            fixture_port,
            ports.port(Consensus, index),
            "canonical validator-{index} consensus port drifted from the harness layout"
        );
    }
}

/// The reported bug: a node index past the committee used to panic. Blocks
/// follow allocation order, so the joiner and followers take the next ones.
#[test]
fn grown_index_does_not_panic() {
    let p = static_ports(4);
    assert_eq!(p.port(Http, 4), NODE_BASE + 4 * BLOCK, "joiner");
    assert_eq!(p.port(Http, 14), NODE_BASE + 5 * BLOCK, "follower1");
    assert_eq!(p.port(Tee, 15), NODE_BASE + 6 * BLOCK + 1, "follower2");
}

/// A scenario never lands on a port the previous one used, however many nodes
/// that one grew.
#[test]
fn scenarios_never_reuse_ports() {
    let p = static_ports(2);
    let first: HashSet<u16> = [0, 1, 2, 14]
        .iter()
        .flat_map(|&i| Service::ALL.map(|svc| p.port(svc, i)))
        .collect();

    p.start_scenario(2).expect("second scenario");
    for i in [0, 1, 2, 14] {
        for svc in Service::ALL {
            assert!(
                !first.contains(&p.port(svc, i)),
                "{svc:?} of node {i} reuses a port from the previous scenario"
            );
        }
    }
}

/// Guards the old layout's http[6] == authrpc[0] == 8551 collision.
#[test]
fn blocks_are_disjoint() {
    let p = static_ports(4);
    let nodes = [0, 1, 2, 3, 4, 14, 15];
    let mut seen = HashSet::new();
    for i in nodes {
        for svc in Service::ALL {
            assert!(seen.insert(p.port(svc, i)), "{svc:?} of node {i} collides");
        }
    }
    assert_eq!(seen.len(), nodes.len() * Service::ALL.len());
}

/// Every `Config` clone must see the same lazily-allocated block.
#[test]
fn clones_share_growth() {
    let a = static_ports(1);
    let b = a.clone();
    assert_eq!(a.port(Http, 20), b.port(Http, 20));
    // A block first allocated through `b` is visible through `a`.
    let via_b = b.port(Http, 21);
    assert_eq!(lock(&a.inner).blocks.get(&21).copied(), Some(via_b));
}

#[test]
fn port_is_memoized() {
    // Memoization is independent of OS socket probing. Keep this unit test
    // deterministic in restricted sandboxes where loopback bind is denied.
    // `scan_shifts_whole_block_past_a_held_port` owns scan behavior.
    let p = Ports::new(false);
    p.start_scenario(1).expect("static allocation");
    assert_eq!(p.port(Http, 9), p.port(Http, 9));
}

#[test]
fn static_layout_preflight_rejects_an_occupied_service_port() {
    let Ok(_held) = TcpListener::bind(("127.0.0.1", NODE_BASE)) else {
        // Some restricted test sandboxes deny loopback bind entirely. The
        // production command still performs the same OS-level preflight.
        return;
    };
    let p = static_ports(1);
    let error = p
        .ensure_available(1)
        .expect_err("occupied validator-0 HTTP port must fail preflight");
    let message = error.to_string();
    assert!(message.contains("validator-0"));
    assert!(message.contains("Http"));
    assert!(message.contains(&NODE_BASE.to_string()));
}

/// Nodes bind their service ports on the wildcard address, not on loopback.
/// BSD `SO_REUSEADDR` semantics let a later `127.0.0.1` bind succeed over a
/// live `0.0.0.0` listener, so a bind-only probe waves a stale node through.
///
/// Takes an ephemeral port rather than the static layout: the sibling
/// preflight test holds [`NODE_BASE`], and nextest runs both at once.
#[test]
fn preflight_rejects_a_wildcard_held_service_port() {
    let Ok(held) = TcpListener::bind(("0.0.0.0", 0)) else {
        // Some restricted test sandboxes deny bind entirely.
        return;
    };
    let occupied = held.local_addr().expect("bound address").port();
    if fits(u64::from(occupied)).is_err() {
        return; // no room for a whole block at this ephemeral port
    }
    // Slot 0 of the block is Http, so the block starts on the held port.
    let p = Ports::from_block_starts(&[occupied]).expect("single block layout");
    let error = p
        .ensure_available(1)
        .expect_err("wildcard-held validator-0 Http port must fail preflight");
    let message = error.to_string();
    assert!(message.contains("validator-0"), "{message}");
    assert!(message.contains("Http"), "{message}");
    assert!(message.contains(&occupied.to_string()), "{message}");
}

#[test]
fn persisted_block_layout_round_trips_every_core_service_port() {
    let original = static_ports(3);
    let starts = original.block_starts(3).unwrap();
    let restored = Ports::from_block_starts(&starts).unwrap();

    for index in 0..3 {
        for service in Service::CORE {
            assert_eq!(
                restored.port(service, index),
                original.port(service, index),
                "validator-{index} {service:?}"
            );
        }
    }
}

#[test]
fn persisted_block_layout_rejects_overlapping_blocks() {
    let error = Ports::from_block_starts(&[NODE_BASE, NODE_BASE + BLOCK - 1])
        .expect_err("overlapping blocks must be rejected");
    assert!(error.to_string().contains("overlap"));
}

#[test]
fn restored_layout_protects_fresh_radicle_ports_without_renumbering_core() {
    let restored = Ports::from_block_starts(&[NODE_BASE, NODE_BASE + BLOCK]).unwrap();
    let ephemeral = ephemeral_port_range().unwrap();
    let mut seen = HashSet::new();
    for index in 0..2 {
        assert_eq!(restored.port(Http, index), NODE_BASE + index as u16 * BLOCK);
        for service in Service::ALL {
            let port = restored.port(service, index);
            assert!(seen.insert(port), "reconstructed services overlap");
            if service.is_radicle() {
                assert!(!ranges_overlap((port, port), ephemeral));
            }
        }
    }
}

/// A busy port shifts only the block that hits it; later blocks follow the
/// cursor, so they never skew relative to each other.
#[test]
fn scan_shifts_whole_block_past_a_held_port() {
    let held = TcpListener::bind(("127.0.0.1", NODE_BASE));
    if held.is_ok() {
        let p = Ports::new(true);
        p.start_scenario(2).expect("scan");
        let first = p.port(Http, 0);
        assert_ne!(first, NODE_BASE, "should skip the held port");
        for service in Service::CORE {
            assert_eq!(
                p.port(service, 0),
                first + service.offset(),
                "block must stay contiguous at {service:?}"
            );
        }
        assert!(
            p.port(Http, 1) - first >= BLOCK,
            "the next block follows the cursor"
        );
        assert_eq!(
            p.port(RadicleStatus, 0),
            p.port(Radicle, 0) + 1,
            "the separate Radicle block stays contiguous"
        );
    }
}

#[test]
fn radicle_skips_the_entire_kernel_ephemeral_interval() {
    for cursor in [49_999, 50_000, 60_999] {
        let ports = Ports::new(false);
        let mut resolver = lock(&ports.inner);
        resolver.radicle_cursor = cursor;
        assert_eq!(
            resolver
                .alloc_radicle_avoiding(Some((50_000, 60_999)))
                .unwrap(),
            61_000
        );
    }
    let ports = Ports::new(false);
    let mut resolver = lock(&ports.inner);
    resolver.radicle_cursor = 49_998;
    assert_eq!(
        resolver
            .alloc_radicle_avoiding(Some((50_000, 60_999)))
            .unwrap(),
        49_998
    );
    assert_eq!(
        resolver
            .alloc_radicle_avoiding(Some((50_000, 60_999)))
            .unwrap(),
        61_000
    );
}

#[test]
fn malformed_kernel_range_is_not_silently_ignored() {
    assert_eq!(
        parse_ephemeral_port_range("32768\t60999\n").unwrap(),
        (32768, 60999)
    );
    for value in [
        "",
        "32768",
        "1 2 3",
        "0 123",
        "60000 50000",
        "1 65536",
        "no ports",
    ] {
        assert!(parse_ephemeral_port_range(value).is_err(), "{value}");
    }
}

#[test]
fn core_and_radicle_never_reissue_each_others_spans() {
    let ports = Ports::new(false);
    let mut resolver = lock(&ports.inner);
    resolver.cursor = 50_000;
    assert_eq!(resolver.block_start(0).unwrap(), 50_000);
    assert_eq!(resolver.radicle_block_start(0).unwrap(), 50_021);
    assert_eq!(resolver.block_start(1).unwrap(), 50_023);
    drop(resolver);
    ports.start_scenario(0).unwrap();
    let mut resolver = lock(&ports.inner);
    assert_eq!(resolver.radicle_block_start(0).unwrap(), 50_044);
}

#[test]
fn final_port_block_is_not_reissued_after_exhaustion() {
    let ports = Ports::new(false);
    let mut resolver = lock(&ports.inner);
    resolver.radicle_cursor = 65_534;
    assert_eq!(resolver.alloc_radicle_avoiding(None).unwrap(), 65_534);
    assert!(resolver.alloc_radicle_avoiding(None).is_err());
    resolver.cursor = 65_515;
    assert!(
        resolver.alloc().is_err(),
        "last core span overlaps issued Radicle ports"
    );
}

#[test]
fn last_core_window_ends_at_maximum_port_and_is_not_reissued() {
    let ports = Ports::new(false);
    let mut resolver = lock(&ports.inner);
    let start = u16::MAX - (BLOCK - 1);
    resolver.cursor = u64::from(start);
    assert_eq!(resolver.alloc().unwrap(), start);
    assert_eq!(resolver.issued, vec![(start, u16::MAX)]);
    assert_eq!(resolver.cursor, u64::from(u16::MAX) + 1);
    assert!(resolver.alloc().is_err());
}

#[test]
fn persisted_final_core_window_round_trips_in_either_order() {
    let last = u16::MAX - (BLOCK - 1);
    for starts in [[NODE_BASE, last], [last, NODE_BASE]] {
        let ports = Ports::from_block_starts(&starts).unwrap();
        assert_eq!(ports.block_starts(2).unwrap(), starts);
        let mut resolver = lock(&ports.inner);
        assert_eq!(resolver.cursor, u64::from(u16::MAX) + 1);
        assert!(resolver.alloc().is_err());
    }
    assert!(Ports::from_block_starts(&[last, last]).is_err());
}

#[test]
fn scanned_radicle_pair_skips_an_occupied_status_port() {
    let held = TcpListener::bind(("127.0.0.1", 0)).expect("bind occupied status fixture");
    let occupied = held.local_addr().unwrap().port();
    let ports = Ports::new(true);
    let mut resolver = lock(&ports.inner);
    resolver.radicle_cursor = u64::from(occupied - 1);
    let start = resolver.alloc_radicle_avoiding(None).unwrap();
    assert!(
        start > occupied,
        "neither peer nor status may use the held port"
    );
}
