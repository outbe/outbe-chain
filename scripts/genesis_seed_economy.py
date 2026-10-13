"""Seed protocol assets, markets and day-indexed economic state."""

from __future__ import annotations
from genesis_seed_codec import (
    METADOSIS_ADDRESS,
    NOD_ADDRESS,
    ORACLE_ADDRESS,
    StorageBuilder,
    TRIBUTE_ADDRESS,
)
from genesis_seed_oracle import seed_oracle

from genesis_seed_codec import (
    VAULT_ROUTER_LIQUIDITY_SOURCES,
    VAULT_ROUTER_LIQUIDITY_TARGETS,
    address_as_u256,
    address_bytes,
    alloc_balance_hex,
    data_slot,
    parse_int,
    timestamp_to_utc_date_key,
    u32_bytes,
)
from genesis_keccak import keccak256


def seed_cycle(storage: "StorageBuilder", genesis_timestamp: int):
    """Seed Cycle slot 2 with the UTC day owned by the genesis block."""
    storage.set_slot(2, timestamp_to_utc_date_key(genesis_timestamp))


def seed_coen(alloc: dict, balances: dict):
    """
    native EVM token alloc layout:
      alloc[addr].balance: U256 COEN unit
    """
    for addr, amount_str in balances.items():
        amount = parse_int(amount_str)
        alloc.setdefault(address_bytes(addr).hex(), {})["balance"] = alloc_balance_hex(
            amount
        )


def tribute_token_id(owner: str, worldwide_day: int) -> bytes:
    """Generate tribute token_id = keccak256(owner_20B ++ wwd_4B)."""
    buf = address_bytes(owner) + u32_bytes(worldwide_day)
    return keccak256(buf)


def day_index_key(day: int, index: int) -> bytes:
    """keccak256(day_4B ++ index_4B) for tribute day index."""
    buf = u32_bytes(day) + u32_bytes(index)
    return keccak256(buf)


def owner_index_key(owner: str, index: int) -> bytes:
    """keccak256(owner_20B ++ index_4B) for tribute/nod owner index."""
    buf = address_bytes(owner) + u32_bytes(index)
    return keccak256(buf)


def seed_tributes(storage: StorageBuilder, tributes: list):
    """
    Tribute storage layout:
      slot 0: total_supply (u64)
      slot 1: mapping(B256 => Address) owners
      slot 2: mapping(B256 => u32) worldwide_days
      slot 3: mapping(B256 => U256) issuance_amounts
      slot 4: mapping(B256 => u32) settlement_currencies
      slot 5: mapping(B256 => U256) nominal_amounts
      slot 6: mapping(u32 => u32) day_tribute_counts
      slot 7: mapping(u32 => U256) day_nominal_amounts
      slot 8: mapping(u32 => bool) day_blocked
      slot 9: mapping(B256 => B256) day_token_ids
      slot 10: mapping(Address => u32) owner_tribute_counts
      slot 11: mapping(B256 => B256) owner_tribute_ids
    """
    # Track per-day and per-owner counters
    day_counts: dict[int, int] = {}
    day_nominals: dict[int, int] = {}
    owner_counts: dict[str, int] = {}

    for tribute in tributes:
        owner = tribute["owner"]
        wwd = tribute["worldwide_day"]
        settlement = parse_int(tribute["issuance_amount"])
        currency = tribute["issuance_currency"]
        nominal = parse_int(tribute["nominal_amount"])

        # Generate token_id
        token_id = tribute_token_id(owner, wwd)

        # Store tribute fields
        storage.set_mapping(1, token_id, address_as_u256(owner))
        storage.set_mapping(2, token_id, wwd)
        storage.set_mapping(3, token_id, settlement)
        storage.set_mapping(4, token_id, currency)
        storage.set_mapping(5, token_id, nominal)

        # Day index (slot 9)
        day_idx = day_counts.get(wwd, 0)
        di_key = day_index_key(wwd, day_idx)
        storage.set_mapping_b256(9, di_key, token_id)
        day_counts[wwd] = day_idx + 1

        # Day nominal accumulator
        day_nominals[wwd] = day_nominals.get(wwd, 0) + nominal

        # Owner index (slot 11)
        owner_lower = owner.lower()
        oi = owner_counts.get(owner_lower, 0)
        oi_key = owner_index_key(owner, oi)
        storage.set_mapping_b256(11, oi_key, token_id)
        owner_counts[owner_lower] = oi + 1

    # Write day counts and nominals
    for wwd, count in day_counts.items():
        storage.set_mapping(6, u32_bytes(wwd), count)
    for wwd, nominal_total in day_nominals.items():
        storage.set_mapping(7, u32_bytes(wwd), nominal_total)

    # Write owner counts
    for owner, count in owner_counts.items():
        storage.set_mapping(10, address_bytes(owner), count)

    # Total supply
    storage.set_slot(0, len(tributes))


def seed_tribute_day_totals(storage: StorageBuilder, days: list[int]):
    """Initialize the Tribute `day_totals` DSL record for OFFERING days so
    callers can use `offerTribute`. `ensure_day_accepts_tributes` requires
    `initialized == true && !is_sealed`. A directly-seeded OFFERING worldwide
    day never ran the metadosis `unseal_day` that normally initializes it.

    `day_totals` is `Map<WorldwideDay, DayTotals>` at TributeContract slot 1.
    The storage_schema cumulative offsets place `total_supply` at slot 0.
    It occupies one slot, so `day_totals` starts at slot 1.
    Tribute bodies no longer occupy EVM storage.
    Within the `DayTotals` record, the field offset is the cumulative slot index
    by `#[attribute(order)]`:
    `initialized`@0, `tribute_count`@1, `tribute_nominal_total_minor`@2,
    `is_sealed`@3 (its `order = 4` only sorts, and the gap at 3 is not reserved).
    So `day_totals[wwd].initialized` is `Mapping(base_slot=1).get(wwd)`. Writing
    1 makes the record exist + initialized, with `is_sealed` left at its `false`
    default."""
    for wwd in days:
        storage.set_mapping(1, u32_bytes(wwd), 1)


def seed_nod_materialization_fifo(storage: StorageBuilder):
    """Initialize the canonical NOD materialization FIFO bounds."""
    # Pinned by `materialization_fifo_slots_match_the_genesis_seeder` in
    # `crates/core/nod/src/adr006_tests.rs`.
    storage.set_slot(13, 1)  # head_sequence
    storage.set_slot(14, 1)  # tail_sequence (next-free)


def seed_metadosis(storage: StorageBuilder, config: dict):
    """
    Metadosis storage layout - MUST track `crates/core/metadosis/src/schema.rs`
    (`#[storage_schema] MetadosisContract`). Attributes occupy slots in declared
    order. A `Map<WorldwideDayKey, WorldwideDay>` consumes one base slot per record
    field, a `Value` one slot, and a `Set` two (length + positions base):

      slot 0:      bootstrap_end_time (Value<u64>)
      slots 1-10:  worldwide_days (Map<u32, WorldwideDay>) - one mapping per field:
                     1 status(u8)            2 day_type(u8)
                     3 forming_start(u64)     4 forming_end(u64)
                     5 lookback_end(u64)      6 offering_end(u64)
                     7 scheduled_process_time(u64)
                     8 metadosis_limit_minor(U256)
                     9 previous_vwap(U256)   10 current_vwap(U256)
      slot 11:     active_wwd_count (Value<u16>)
      slots 12-13: active_wwd (Set<WorldwideDayKey>) - OZ enumerable set:
                     12 = length + value array at keccak256(be32(12))+i
                     13 = positions base, position(wwd) = index + 1 (0 = absent)
      slot 14+:    closed_wwd (Deque<WorldwideDayKey>)

    The active_wwd Set is what `get_active_wwd_by_status` (and therefore the tribute
    OFFERING lookup) reads. The seeder MUST populate it in the enumerable-set layout
    above. Seeding only the day record (slots 1-10) leaves the day invisible to the
    active scan, and every offer reverts "no worldwide day is OFFERING".
    """
    wwds = config.get("worldwide_days", [])

    # active_wwd is a Set at base slot 12 (schema order 3, right after the
    # active_wwd_count Value at slot 11). Its positions mapping lives at base + 1.
    ACTIVE_WWD_BASE = 12

    for idx, entry in enumerate(wwds):
        wwd = entry["wwd"]
        wwd_key = u32_bytes(wwd)

        storage.set_mapping(1, wwd_key, entry.get("status", 0))
        storage.set_mapping(2, wwd_key, entry.get("day_type", 0))
        storage.set_mapping(3, wwd_key, entry.get("forming_start", 0))
        storage.set_mapping(4, wwd_key, entry.get("forming_end", 0))
        storage.set_mapping(5, wwd_key, entry.get("lookback_end", 0))
        storage.set_mapping(6, wwd_key, entry.get("offering_end", 0))
        storage.set_mapping(7, wwd_key, entry.get("scheduled_process_time", 0))

        # slot 8 = metadosis_limit_minor (per-day mint cap), 9 = previous_vwap,
        # 10 = current_vwap - schema field order.
        day_limit = parse_int(entry.get("day_limit", "0"))
        if day_limit > 0:
            storage.set_mapping(8, wwd_key, day_limit)
        storage.set_mapping(9, wwd_key, parse_int(entry.get("previous_vwap", "0")))
        storage.set_mapping(10, wwd_key, parse_int(entry.get("current_vwap", "0")))

        # Insert into the active_wwd Set: value-array entry + 1-indexed position.
        storage.set_slot(data_slot(ACTIVE_WWD_BASE) + idx, wwd)
        storage.set_mapping(ACTIVE_WWD_BASE + 1, u32_bytes(wwd), idx + 1)

    # Set length (slot 12) and the separate active_wwd_count Value (slot 11).
    storage.set_slot(ACTIVE_WWD_BASE, len(wwds))
    storage.set_slot(11, len(wwds))

    # Bootstrap end time
    bootstrap_end = config.get("bootstrap_end_time", 0)
    if bootstrap_end:
        storage.set_slot(0, bootstrap_end)


def _seed_address_set_with_types(
    storage: StorageBuilder,
    *,
    set_base: int,
    type_map_slot: int,
    entries: list[tuple[str, int]],
):
    """
    Seed an OZ-style enumerable `Set<Address>` plus its parallel
    `mapping(Address => u8)` type map, matching the Rust `StorageSet` layout:
      set_base       = element count (length)
      set_base       -> value array at keccak256(be32(set_base)) + index
      set_base + 1   = positions mapping (1-indexed, 0 = absent)
      type_map_slot  = mapping(Address => u8) discriminant
    """
    for idx, (addr, type_value) in enumerate(entries):
        addr_bytes = address_bytes(addr)
        # Set: value-array entry (address right-aligned in a 32-byte word) +
        # 1-indexed position.
        storage.set_slot(data_slot(set_base) + idx, address_as_u256(addr))
        storage.set_mapping(set_base + 1, addr_bytes, idx + 1)
        # Parallel discriminant map.
        storage.set_mapping(type_map_slot, addr_bytes, type_value)
    storage.set_slot(set_base, len(entries))


def seed_vault_router(storage: StorageBuilder, owner_address: str):
    """
    VaultRouter storage layout (see crates/core/vaultrouter/src/schema.rs):
      slot 0:      owner (admin)
      slots 1-2:   assets (Set) - written at runtime by addVault
      slot 3:      asset_vaults (Map) - written at runtime by addVault
      slots 4-5:   liquidity_sources (Set<Address>)
      slot 6:      liquidity_source_types (Map<Address, u8>)
      slots 7-8:   liquidity_targets (Set<Address>)
      slot 9:      liquidity_target_types (Map<Address, u8>)

    Genesis sets the owner and pre-registers the default liquidity source/target
    registry for the factory precompiles. This configures and restricts the
    Solidity deposit / withdraw ABI path. The reserve vault still requires
    registration after deployment through `addVault`. The
    in-process api callers bypass this registry and declare their discriminant
    directly. Owner mirrors the ValidatorSet owner pattern (slot 0).
    """
    storage.set_slot(0, address_as_u256(owner_address))

    _seed_address_set_with_types(
        storage,
        set_base=4,
        type_map_slot=6,
        entries=VAULT_ROUTER_LIQUIDITY_SOURCES,
    )
    _seed_address_set_with_types(
        storage,
        set_base=7,
        type_map_slot=9,
        entries=VAULT_ROUTER_LIQUIDITY_TARGETS,
    )


def clear_seeded_metadosis_days(seed: dict) -> None:
    """Let block-1 create the first WorldwideDay from protocol timings."""
    metadosis = seed.get("metadosis")
    if metadosis is None:
        return
    if not isinstance(metadosis, dict):
        raise ValueError("seed metadosis must be a JSON object")
    metadosis["worldwide_days"] = []


def override_worldwide_day(seed: dict, day: int) -> None:
    """Retarget every worldwide-day reference in a seed to `day` (YYYYMMDD), in
    place: metadosis worldwide_days[].wwd, oracle scurve_seeds[].peak_day, and
    nods[].worldwide_day.

    A localnet must boot on its genesis (current) date. The metadosis runtime
    derives the active day each block from
    `WorldwideDay::from_timestamp(block.timestamp)`. A seeded day that differs
    from the genesis wall-clock day leaves two active worldwide days.
    The seeded day conflicts with the runtime-created "today", and consensus stalls. This
    function leaves other day fields (status, offering window, limits) as authored
    and retargets only the calendar key. The OFFERING window that the seed declares
    (forming in the past, offering_end far future) keeps the day OFFERING at the
    new date.
    """
    for w in seed.get("metadosis", {}).get("worldwide_days", []):
        w["wwd"] = day
    for s in seed.get("oracle", {}).get("scurve_seeds", []):
        s["peak_day"] = day


def _seed_economic_state(seed, alloc):
    # Seed Tributes
    if "tributes" in seed:
        tribute_storage = StorageBuilder()
        seed_tributes(tribute_storage, seed["tributes"])
        # Initialize day_totals for OFFERING worldwide days (status 2) so callers can use offerTribute.
        # A directly seeded OFFERING day never ran the metadosis unseal_day that normally initializes day_totals.
        offering_days = [
            entry_wd["wwd"]
            for entry_wd in seed.get("metadosis", {}).get("worldwide_days", [])
            if entry_wd.get("status", 0) == 2
        ]
        seed_tribute_day_totals(tribute_storage, offering_days)
        entry = alloc[TRIBUTE_ADDRESS]
        entry.setdefault("storage", {}).update(tribute_storage.entries)
        print(
            f"  Tribute: {len(seed['tributes'])} tributes, "
            f"{len(offering_days)} offering day_totals init, "
            f"{len(tribute_storage.entries)} storage entries"
        )

    # Initialize the canonical materialization FIFO. NOD bodies live in
    # compressed-entity storage, and the seeder does not write them into EVM slots.
    nod_storage = StorageBuilder()
    seed_nod_materialization_fifo(nod_storage)
    entry = alloc[NOD_ADDRESS]
    entry.setdefault("storage", {}).update(nod_storage.entries)
    print(f"  Nod: {len(nod_storage.entries)} storage entries")

    # Seed Metadosis
    if "metadosis" in seed:
        meta_storage = StorageBuilder()
        seed_metadosis(meta_storage, seed["metadosis"])
        entry = alloc[METADOSIS_ADDRESS]
        entry.setdefault("storage", {}).update(meta_storage.entries)
        wwds = seed["metadosis"].get("worldwide_days", [])
        print(
            f"  Metadosis: {len(wwds)} worldwide days, "
            f"{len(meta_storage.entries)} storage entries"
        )

    # Seed Oracle
    if "oracle" in seed:
        oracle_storage = StorageBuilder()
        seed_oracle(oracle_storage, seed["oracle"])
        entry = alloc[ORACLE_ADDRESS]
        entry.setdefault("storage", {}).update(oracle_storage.entries)
        pairs = seed["oracle"].get("pairs", [])
        print(
            f"  Oracle: {len(pairs)} pairs, "
            f"{len(oracle_storage.entries)} storage entries"
        )
