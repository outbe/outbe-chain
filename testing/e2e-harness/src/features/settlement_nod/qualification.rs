//! Qualify the NOD with finalized Oracle prices and bounded retries.

use super::*;

struct NodQualification {
    id: WwdEntityId,
    owner: Address,
    floor: U256,
    first_full_day: u32,
    port: u16,
    ports: Vec<u16>,
    rate: U256,
}
struct ClosedUtcDay {
    day: u32,
    target: u64,
    deadline: Instant,
    boundary: u8,
}
enum DayQualification {
    Qualified,
    NextDay(u64),
}

pub(in super::super) fn qualify_public_nod(
    world: &mut World,
    id: WwdEntityId,
    owner: Address,
    floor: U256,
    issued_at: u64,
) {
    assert_ne!(
        issued_at, 0,
        "public Nod must have a sealed issuance timestamp"
    );
    let first_full_day = outbe_primitives::time::first_full_day(issued_at);
    let port = world.validators.primary_port();
    let ports = world.validators.committee_ports();
    let head = world.rpc.head(port).expect("successor qualification head");
    world
        .rpc
        .wait_finalized_checkpoint(&ports, head, 120)
        .expect("qualification precondition finality");
    if successor_is_qualified(world, owner, id, head) {
        return;
    }
    let rate = floor
        .checked_mul(U256::from(2))
        .expect("successor qualification quote");
    assert!(rate > floor);
    let qualification = NodQualification {
        id,
        owner,
        floor,
        first_full_day,
        port,
        ports,
        rate,
    };
    let mut first_boundary_day = None;
    // The first closed day can include prices below the NOD floor.
    // The next complete UTC day uses only the declared quote.
    for boundary in 0..2 {
        let closed = close_qualification_day(world, &qualification, first_boundary_day, boundary);
        match wait_qualification(world, &qualification, closed) {
            DayQualification::Qualified => return,
            DayQualification::NextDay(day) => first_boundary_day = Some(day),
        }
    }
    panic!("successor remained unqualified after two closed UTC days");
}

fn close_qualification_day(
    world: &mut World,
    qualification: &NodQualification,
    first_boundary_day: Option<u64>,
    boundary: u8,
) -> ClosedUtcDay {
    let port = qualification.port;
    let ports = &qualification.ports;
    let rate = qualification.rate;
    crate::features::price_oracle::publish_controlled_quote(world, rate);
    let publication = world
        .price_oracle
        .last_oracle_block()
        .expect("fresh high quote finalized");
    let published_at = world
        .rpc
        .block_timestamp(port, publication)
        .expect("qualification quote timestamp");
    let now = world
        .rpc
        .latest_block_timestamp(port)
        .expect("qualification clock");
    assert_eq!(
        published_at / 86_400,
        now / 86_400,
        "high quote must finalize within the day being closed"
    );
    if let Some(expected_day) = first_boundary_day {
        assert_eq!(
            published_at / 86_400,
            expected_day,
            "fallback must close the complete subsequent UTC day"
        );
    }
    let target = (now / 86_400 + 1)
        .checked_mul(86_400)
        .and_then(|v| v.checked_add(1))
        .expect("next UTC boundary");
    let closed_day = outbe_primitives::time::timestamp_to_date_key(now);
    let (_, _, height, pending) =
        crate::features::ocomp::restart_committee_at_logical_time(world, target);
    for &peer in ports {
        assert!(world.rpc.wait_finalized_at_least(peer, height, 240));
    }
    let deadline = Instant::now() + Duration::from_secs(120);
    if let Some(pending) = pending {
        while !crate::features::price_oracle::observe_pending_publication(world, &pending) {
            assert!(
                Instant::now() < deadline,
                "post-jump feeder did not finalize"
            );
            sleep(Duration::from_millis(500));
        }
    }
    ClosedUtcDay {
        day: closed_day,
        target,
        deadline,
        boundary,
    }
}

fn wait_qualification(
    world: &World,
    qualification: &NodQualification,
    closed: ClosedUtcDay,
) -> DayQualification {
    let port = qualification.port;
    let ports = &qualification.ports;
    let owner = qualification.owner;
    let id = qualification.id;
    let floor = qualification.floor;
    let rate = qualification.rate;
    let first_full_day = qualification.first_full_day;
    let ClosedUtcDay {
        day: closed_day,
        target,
        deadline,
        boundary,
    } = closed;
    loop {
        let latest = world
            .rpc
            .finalized(port)
            .expect("qualification finalized height");
        let checkpoint = world
            .rpc
            .wait_finalized_checkpoint(ports, latest, 120)
            .expect("common closed-day qualification checkpoint");
        let values = qualification_vwaps(world, qualification, closed_day, checkpoint.height);
        if let Some(vwap) = matching_vwap(&values) {
            if boundary == 1 {
                assert_eq!(
                    vwap, rate,
                    "the full fallback UTC day must contain only the declared high quote"
                );
            }
            if closed_day < first_full_day || vwap <= floor {
                assert_eq!(boundary, 0, "isolated high-price day must exceed Nod floor");
                eprintln!("settlement_evidence kind=qualification_fallback day={closed_day} first_full_day={first_full_day} vwap={vwap} floor={floor}");
                return DayQualification::NextDay(target / 86_400);
            }
            if successor_is_qualified(world, owner, id, checkpoint.height) {
                eprintln!("settlement_evidence kind=successor_qualified day={closed_day} vwap={vwap} floor={floor} boundary={} height={}", boundary + 1, checkpoint.height);
                return DayQualification::Qualified;
            }
        }
        assert!(Instant::now() < deadline,
                "successor qualification failed: boundary={} day={closed_day} floor={floor} rate={rate} finalized={} observed_vwaps={values:?}",
                boundary + 1, checkpoint.height);
        sleep(Duration::from_millis(500));
    }
}

fn qualification_vwaps(
    world: &World,
    qualification: &NodQualification,
    closed_day: u32,
    height: u64,
) -> Vec<Option<U256>> {
    let ports = &qualification.ports;
    ports
        .iter()
        .map(|&peer| {
            eth::read_call_at(
                &world.rpc.url(peer),
                outbe_primitives::addresses::ORACLE_ADDRESS,
                &eth::IOracle::getUtcDayVwapCall {
                    base: Address::ZERO,
                    quote: outbe_primitives::asset_type::currency_address(USD_ISO),
                    utcDay: closed_day,
                },
                height,
            )
        })
        .collect::<Vec<_>>()
}

fn matching_vwap(values: &[Option<U256>]) -> Option<U256> {
    values
        .first()
        .copied()
        .flatten()
        .filter(|vwap| values.iter().all(|value| *value == Some(*vwap)))
}
