"""Seed governance and system precompile state."""

from __future__ import annotations

import os

from genesis_keccak import keccak256
from genesis_seed_alloc import seed_governments_descriptor, seed_code_registry
from genesis_seed_codec import (
    ACCOUNTING_PROGRESS_ADDRESS,
    GOVERNANCE_ADDRESS,

    MARKER_CODE,
    CODE_REGISTRY_ADDRESS,
    TEE_REGISTRY_ADDRESS,
    ZEROFEE_ADDRESS,
    StorageBuilder,
    compute_tee_policy_hash,
    parse_int,
    u64_bytes,
    write_storage_bytes,
)


def seed_tee_policy(genesis: dict, alloc: dict, seed: dict):
    """Seed the genesis TEE attestation policy (WS-B), if `tee_policy` is present
    in the seed config.

    Writes two places:
      1. `TeeRegistry` (0xEE0A) slot 2 = `policy_hash` - the consensus-critical,
         deterministic gate the Phase 3b `TeeBootstrap` handler reads from EVM
         state. The account also gets marker bytecode so the slot survives
         EIP-161 cleanup until block 1.
      2. `config.teePolicy` - the node reads it at startup to build the host
         structural key/measurement consistency checks at development connect.

    No-op when `tee_policy` is absent: genesis is unchanged and the handler skips
    measurement enforcement (slot 2 == ZERO).
    """
    policy = seed.get("tee_policy")
    if not policy:
        return
    allowed_mrsigner = policy.get("allowed_mrsigner", [])
    allowed_mrenclave = policy.get("allowed_mrenclave", [])
    min_isv_svn = parse_int(policy.get("min_isv_svn", 0))
    policy_hash = compute_tee_policy_hash(
        allowed_mrsigner, allowed_mrenclave, min_isv_svn
    )

    storage = StorageBuilder()
    storage.set_raw_slot_hex(2, "0x" + policy_hash.hex())
    entry = alloc.setdefault(TEE_REGISTRY_ADDRESS, {})
    entry["code"] = MARKER_CODE
    entry.setdefault("balance", "0x0")
    entry.setdefault("storage", {}).update(storage.entries)

    genesis.setdefault("config", {})["teePolicy"] = {
        "allowed_mrsigner": allowed_mrsigner,
        "allowed_mrenclave": allowed_mrenclave,
        "min_isv_svn": min_isv_svn,
    }
    print(
        f"  teePolicy: {len(allowed_mrsigner)} mrsigner, {len(allowed_mrenclave)} mrenclave, "
        f"min_isv_svn={min_isv_svn}, policy_hash=0x{policy_hash.hex()}"
    )


def seed_zerofee(storage: StorageBuilder):
    """
    ZeroFee paymaster storage layout:
      slot 0: schema version (uint32) - pinned at 1 for the initial
              `Map<Address, u64> counter` layout. The macro's
              `counter` Map keys are `keccak256(addr || base_slot)` so
              they never collide with slot 0 even though `counter`
              nominally uses slot 0 as the base_slot for keccak.

    The README rule "All precompiles ... storage versioned (slot 0 = version)"
    requires the slot-0 schema marker. A
    future layout migration would bump this value and key off it from
    the runtime.
    """
    storage.set_slot(0, 1)


def seed_accounting_progress(storage: StorageBuilder):
    """
    Accounting progress storage layout (V2):
      slot 0: last_accounted_block_number (u64) - pre-V2 genesis is `0`
              meaning Phase 1 has not yet processed any block. The first
              certified-parent accounting begin-zone system transaction
              advances this slot for block N >= 2.

    Genesis V2 requires this slot to be explicitly written so the resulting
    storage map contains the canonical zero word, matching the Rust schema
    `outbe_cycle::schema::Accounting::last_accounted_block_number`.
    """
    storage.set_slot(0, 0)


def seed_governance(storage: StorageBuilder, canon_dir: str | None):
    """
    Governance storage layout. Each seeded field occupies one slot before the
    two record maps in the schema. Their slot indices stay stable as the Oip/Gip
    record types grow. A `Map<K, Record>` reserves `Record::SLOTS` contiguous base slots.
    The Rust-side
    `storage_layout_matches_seeder` test pins these indices.

      slot 0:  meta_canon text        (StorageBytes, long data at keccak256(0))
      slot 1:  meta_canon_version     (u64)
      slot 2:  meta_canon_hash        (B256 = keccak256(text))
      slot 3:  meta_canon_revisions   Map<u64, B256>
      slot 4:  canon text             (StorageBytes, long data at keccak256(4))
      slot 5:  canon_version          (u64)
      slot 6:  canon_hash             (B256)
      slot 7:  canon_revisions        Map<u64, B256>
      slot 8:  next_oip_id            (u64, default 0 - not seeded)
      slot 9:  next_gip_id            (u64, default 0 - not seeded)
      slot 10: reserved (former genesis authorities copy, never written)
      slot 11: oips                   Map<U256, Oip>   (not seeded, empty)
      slot 17: gips                   Map<U256, Gip>   (not seeded, empty)

    Write access is not seeded: the precompile checks the caller against the
    live ValidatorSet (ACTIVE status) at call time.
    Genesis uses the canon / meta-canon texts from `canon_dir/{metacanon.md,canon.md}`
    at version 1 when present. When absent, the texts stay empty and any ACTIVE
    validator performs the first `updateCanon` post-genesis (version 0 -> 1).

    Returns `(meta_seeded, canon_seeded)`.
    """

    def _seed_text(
        text_slot: int,
        version_slot: int,
        hash_slot: int,
        revisions_base: int,
        data: bytes,
    ):
        write_storage_bytes(storage, text_slot, data)
        storage.set_slot(version_slot, 1)
        h = keccak256(data)
        storage.set_raw_slot_hex(hash_slot, "0x" + h.hex())
        storage.set_mapping_b256(revisions_base, u64_bytes(1), h)

    meta_seeded = False
    canon_seeded = False
    if canon_dir and os.path.isdir(canon_dir):
        meta_path = os.path.join(canon_dir, "metacanon.md")
        canon_path = os.path.join(canon_dir, "canon.md")
        if os.path.isfile(meta_path):
            with open(meta_path, "rb") as f:
                _seed_text(0, 1, 2, 3, f.read())
            meta_seeded = True
        if os.path.isfile(canon_path):
            with open(canon_path, "rb") as f:
                _seed_text(4, 5, 6, 7, f.read())
            canon_seeded = True

    return meta_seeded, canon_seeded


def _seed_system_state(genesis, seed, alloc, canon_dir):
    # Governance: seed the canon / meta-canon texts. Write access comes from the
    # live ValidatorSet, not from genesis. Canon texts default to <script-dir>/canon.
    canon_dir = canon_dir or os.path.join(
        os.path.dirname(os.path.abspath(__file__)), "canon"
    )
    governance_storage = StorageBuilder()
    meta_seeded, canon_seeded = seed_governance(governance_storage, canon_dir)
    if governance_storage.entries:
        alloc[GOVERNANCE_ADDRESS].setdefault("storage", {}).update(
            governance_storage.entries
        )
    print(
        f"  Governance: meta-canon={'seeded' if meta_seeded else 'empty'}, "
        f"canon={'seeded' if canon_seeded else 'empty'}, "
        f"{len(governance_storage.entries)} storage entries"
    )

    # V2 Phase 1 accounting progress (slot 0 = 0). Genesis always seeds it,
    # regardless of validator count. The executor needs the marker bytecode and
    # an explicit slot 0 = 0 word to record `last_accounted_block_number` with EIP-161-safe storage.
    accounting_storage = StorageBuilder()
    seed_accounting_progress(accounting_storage)
    alloc[ACCOUNTING_PROGRESS_ADDRESS].setdefault("storage", {}).update(
        accounting_storage.entries
    )
    print(
        f"  AccountingProgress: slot 0 = 0, "
        f"{len(accounting_storage.entries)} storage entries"
    )

    if "code_registry" in seed:
        code_registry_storage = StorageBuilder()
        seed_code_registry(code_registry_storage, seed["code_registry"])
        seed_governments_descriptor(
            genesis, code_registry_storage, seed["code_registry"]
        )
        alloc[CODE_REGISTRY_ADDRESS].setdefault("storage", {}).update(
            code_registry_storage.entries
        )
        print(
            "  CodeRegistry: maxRepositories="
            f"{seed['code_registry']['max_repositories']}"
        )

    # ZeroFee paymaster: slot 0 = schema version (1). This honors the README
    # rule "All precompiles storage versioned (slot 0 = version)". It also
    # lets a future migration probe slot 0 to decide whether to apply
    # a layout transformation. The `counter` Map keys are keccak-derived
    # and never write to slot 0 directly, so the version marker has no
    # collision risk.
    zerofee_storage = StorageBuilder()
    seed_zerofee(zerofee_storage)
    alloc[ZEROFEE_ADDRESS].setdefault("storage", {}).update(zerofee_storage.entries)
    print(
        f"  ZeroFee: slot 0 = 1 (schema version), "
        f"{len(zerofee_storage.entries)} storage entries"
    )

    # TEE attestation policy (WS-B): seeds TeeRegistry slot 2 (policy_hash) +
    # config.teePolicy, but only when `tee_policy` is present in the seed config.
    seed_tee_policy(genesis, alloc, seed)
