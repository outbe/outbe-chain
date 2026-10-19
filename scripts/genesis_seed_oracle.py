"""Validate and seed the Oracle registry and pricing state."""

from __future__ import annotations

from genesis_seed_codec import (
    StorageBuilder,
    address_as_u256,
    address_bytes,
    data_slot,
    SECONDS_PER_DAY,
    parse_int,
    u32_bytes,
    wwd_to_day_timestamp,
)
from genesis_seed_codec import address_pair, asset_address, is_iso_asset_address


def seed_oracle(storage: StorageBuilder, config: dict):
    """
    Oracle storage layout:
      slots 0-7: config
      slot 8: pair_count
      slot 9: retired hole (was mapping(pair_id => pair_hash))
      slot 10: mapping(pair => ordinal)
      slot 11: mapping(pair => is_vote_target)
      slots 12-14: exchange_rate / block / timestamp, keyed by pair_index
        (the 1-based ordinal from slot 10), not by the pair itself
      slot 15: feeder delegations
      slots 32-33: retired holes (were protected validators)
      slot 43: mapping(pair_index => AddressPair), a two-word value and the only
        way to enumerate the registry. Slots 40-42, 44, 45 and 46 are retired
        settlement holes. The settlement pair is address_pair("COEN", "<iso>").
      slot 55: reference_currencies (StorageVec<u16>)
      slot 60: retired policy-rate mapping
      slots 74-75: policy_rate_currencies / policy_rate
    """
    pairs = config.get("pairs", [])
    validated_pairs = _validate_oracle_pairs(pairs)
    # Validate the complete registry before writing any Oracle slot. This keeps
    # direct seeding and create_genesis.py fail-closed on invalid input.
    _seed_oracle_config(storage, config.get("config", {}), pairs)
    pair_ids = _seed_oracle_registry(storage, config, validated_pairs)
    _seed_oracle_scurves(storage, config, pair_ids)
    _seed_oracle_currencies(storage, config)


def _validate_oracle_pairs(pairs):
    pair_keys: dict[tuple[str, str], bytes] = {}
    validated_pairs = []

    for idx, pair in enumerate(pairs, start=1):
        base = pair["base"]
        quote = pair["quote"]
        base_address = asset_address(base)
        quote_address = asset_address(quote)
        if base_address == quote_address:
            raise ValueError(
                f"oracle pair must contain two different assets: same asset {base}/{quote}"
            )
        h = address_pair(base, quote)
        key = (base, quote)
        if key in pair_keys:
            raise ValueError(f"duplicate oracle pair: {base}/{quote}")
        # The key is order-independent, so the inverse is the same pair.
        if h in pair_keys.values():
            raise ValueError(f"oracle pair already registered inverted: {base}/{quote}")
        # This seeder writes slot 43 directly and bypasses `register_pair`. So it
        # owes the same directional invariant: generic markets preserve their
        # configured orientation, while COEN/ISO is always COEN base, ISO quote.
        if is_iso_asset_address(base_address) and quote_address == bytes(20):
            raise ValueError(
                f"oracle COEN/ISO pair must use COEN base and ISO quote: {base}/{quote}"
            )
        pair_keys[key] = h
        validated_pairs.append((idx, pair, base, quote, h))

    return validated_pairs


def _seed_oracle_config(storage, cfg, pairs):
    storage.set_slot(0, parse_int(cfg.get("vote_period", 2)))
    storage.set_slot(1, parse_int(cfg.get("reward_band", "20000000000000000")))
    penalties_enabled = cfg.get("penalties_enabled", True)
    if penalties_enabled:
        min_valid_per_window = cfg.get("min_valid_per_window", "50000000000000000")
        slash_fraction = cfg.get("slash_fraction", "0")
    else:
        min_valid_per_window = "0"
        slash_fraction = "0"
    storage.set_slot(2, parse_int(cfg.get("slash_window", 96)))
    storage.set_slot(3, parse_int(min_valid_per_window))
    storage.set_slot(4, parse_int(slash_fraction))
    storage.set_slot(5, parse_int(cfg.get("lookback_duration", SECONDS_PER_DAY)))
    storage.set_slot(6, 1 if cfg.get("enabled", True) else 0)
    storage.set_slot(7, 1 if cfg.get("initialized", True) else 0)
    storage.set_slot(8, len(pairs))


def _seed_oracle_registry(storage, config, validated_pairs):
    pair_ids: dict[tuple[str, str], int] = {}
    for idx, pair, base, quote, h in validated_pairs:
        key = (base, quote)
        pair_ids[key] = idx

        storage.set_mapping(10, h, idx)
        storage.set_mapping(11, h, 1 if pair.get("vote_target", True) else 0)
        # pair_by_index (macro slot 43), configured orientation.
        storage.set_mapping_pair(43, u32_bytes(idx), base, quote)

        rate = parse_int(pair.get("initial_rate", "0"))
        if rate:
            storage.set_mapping(12, u32_bytes(idx), rate)
            storage.set_mapping(
                13, u32_bytes(idx), parse_int(pair.get("initial_block", 0))
            )
            storage.set_mapping(
                14, u32_bytes(idx), parse_int(pair.get("initial_timestamp", 0))
            )

    for rate_entry in config.get("initial_rates", []):
        key = (rate_entry["base"], rate_entry["quote"])
        idx = pair_ids.get(key)
        if idx is None:
            raise ValueError(f"initial rate pair is not registered: {key[0]}/{key[1]}")
        storage.set_mapping(12, u32_bytes(idx), parse_int(rate_entry["rate"]))
        storage.set_mapping(13, u32_bytes(idx), parse_int(rate_entry.get("block", 0)))
        storage.set_mapping(
            14, u32_bytes(idx), parse_int(rate_entry.get("timestamp", 0))
        )

    for delegation in config.get("feeder_delegations", []):
        validator = delegation["validator"]
        feeder = delegation["feeder"]
        storage.set_mapping(15, address_bytes(validator), address_as_u256(feeder))

    if config.get("protected_validators"):
        raise ValueError(
            "oracle.protected_validators was removed: every validator follows "
            "the same slash-window rules"
        )

    if config.get("settlement_currencies"):
        raise ValueError(
            "oracle.settlement_currencies was removed: register the COEN/<iso> "
            "pair instead, and list the ISO under oracle.reference_currencies"
        )

    return pair_ids


def _seed_oracle_scurves(storage, config, pair_ids):
    # S-curve genesis seeds (macro slots 34-38). `resolve_tribute_price` reads
    # `max(per-day VWAP, S-curve)`. Pre-seeded OFFERING days have no runtime-
    # computed per-day VWAP. Without an S-curve entry the price is 0 and
    # `offerTribute` reverts with `NominalPriceUnavailable`. Each seed gives a
    # pair a peak at a worldwide day so days within the S-curve period resolve.
    scurve_seeds = config.get("scurve_seeds", [])
    if scurve_seeds:
        storage.set_slot(34, len(scurve_seeds))  # scurve_count
        storage.set_slot(38, 0)  # scurve_oldest_idx
        for idx, sc in enumerate(scurve_seeds):
            pair = (sc["pair_base"], sc["pair_quote"])
            if pair_ids.get(pair) is None:
                raise ValueError(
                    f"scurve seed pair is not registered: {pair[0]}/{pair[1]}"
                )
            peak_day_ts = wwd_to_day_timestamp(parse_int(sc["peak_day"]))
            # scurve_pair (35): a two-word pair value, base then base+1.
            storage.set_mapping_pair(35, u32_bytes(idx), pair[0], pair[1])
            storage.set_mapping(36, u32_bytes(idx), peak_day_ts)  # scurve_peak_day
            storage.set_mapping(
                37, u32_bytes(idx), parse_int(sc["peak_price"])
            )  # scurve_peak_price


def _seed_oracle_currencies(storage, config):
    # Reference currencies and annual policy rates are independent registries.
    # Both lists are canonical ascending ISO order. Slot 60 is retired.
    DEFAULT_USD_CURRENCY_RATE = 36_300  # 3.63% at scale 1e6
    reference_currencies = config.get(
        "reference_currencies",
        [156, 344, 392, 826, 840, 978],
    )
    reference_currencies = [parse_int(iso_code) for iso_code in reference_currencies]
    if any(iso_code == 0 for iso_code in reference_currencies):
        raise ValueError("oracle reference iso_code must be non-zero")
    if reference_currencies != sorted(set(reference_currencies)):
        raise ValueError("oracle reference currencies must be sorted and unique")
    if 840 not in reference_currencies:
        raise ValueError("oracle reference currencies must include USD 840")
    storage.set_slot(55, len(reference_currencies))
    for i, iso_code in enumerate(reference_currencies):
        storage.set_raw_slot(data_slot(55) + i, iso_code)

    policy_rates = config.get(
        "policy_rates",
        [{"iso_code": 840, "annual_rate_1e6": DEFAULT_USD_CURRENCY_RATE}],
    )
    parsed_policy_rates = [
        (parse_int(entry["iso_code"]), parse_int(entry["annual_rate_1e6"]))
        for entry in policy_rates
    ]
    if any(iso_code == 0 for iso_code, _ in parsed_policy_rates):
        raise ValueError("oracle policy iso_code must be non-zero")
    if any(rate == 0 for _, rate in parsed_policy_rates):
        raise ValueError("oracle policy rate must be non-zero")
    policy_isos = [iso_code for iso_code, _ in parsed_policy_rates]
    if policy_isos != sorted(set(policy_isos)):
        raise ValueError("oracle policy rates must be sorted and unique")
    storage.set_slot(74, len(parsed_policy_rates))
    for i, (iso_code, rate) in enumerate(parsed_policy_rates):
        storage.set_raw_slot(data_slot(74) + i, iso_code)
        storage.set_mapping(75, u32_bytes(iso_code), rate)
