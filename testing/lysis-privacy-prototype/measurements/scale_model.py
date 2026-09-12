#!/usr/bin/env python3
"""Derived scale model. This is arithmetic on measured costs, not a load test."""
import json
from pathlib import Path

base = Path(__file__).resolve().parent
r = json.loads((base / "rust_results.json").read_text())
s = json.loads((base / "seal_results.json").read_text())
w = json.loads((base / "worker_results.json").read_text())
issue, claim, withdraw = r["arithmetic_proofs"]
tribute_layout = {"version":1,"id":32,"owner":20,"day":4,"currencies":40,
                  "effective_reference_price":32,"issuance_commitments":128,
                  "nominal_commitments":128,"exclude":1,"source_binding":32}
nod_layout = {"version":1,"source_id":32,"owner":20,"day":4,"league":2,
              "nominal_commitments":128,"fraction":32,"entry_price":32,
              "floor_price":32,"reference_currency":20,"expiry":8,"params_root":32}
assert sum(nod_layout.values()) == w["nod_encoding"]["record_bytes"]
out = {"kind":"derived_model_not_network_tps","units":"decimal bytes/GB/TB; seconds",
       "tribute_proposed_body_fields":tribute_layout,"tribute_body_bytes":sum(tribute_layout.values()),
       "nod_proposed_body_fields":nod_layout,"nod_body_bytes":sum(nod_layout.values()),
       "private_gratis_value_bytes_without_owner_key_or_db_overhead":136,
       "profiles":[]}
for count in [1_000_000,1_000_000_000]:
    target=count/(50*3600)
    for vss in r["vss"]:
        t=vss["threshold"]
        public_per=sum(tribute_layout.values())+issue["proof_bytes"]+vss["extra_coeff_bytes_excluding_existing_amount_commitments"]
        check_ms=issue["verify"]["median_ms"]+vss["verify_one_recipient"]["median_ms"]
        out["profiles"].append({
            "tributes":count,"n":vss["n"],"threshold":t,"target_per_second_in_50h":target,
            "tribute_modeled_public_bytes_each":public_per,
            "public_total_bytes":public_per*count,
            "private_fanout_total_bytes":vss["private_bytes_all_recipients"]*count,
            "per_replica_input_private_bytes":vss["private_bytes_per_recipient"]*count,
            "proof_plus_share_check_single_thread_per_second":1000/check_ms,
            "cpu_core_equivalents_per_replica_at_target_rate_arithmetic_plus_share_check_only":target*check_ms/1000,
            "client_proof_plus_vss_deal_ms":issue["proof"]["median_ms"]+vss["deal_generation_all_recipients"]["median_ms"],
            "nod_bodies_bytes":sum(nod_layout.values())*count,
            "only_one_aggregate_private_state_per_replica_bytes":vss["aggregate_private_state_per_recipient_bytes"],
            "aggregate_public_coefficients_bytes":vss["aggregate_public_coefficients_bytes"],
            "late_grouping_per_record_retained_private_state_per_replica_bytes":256*count,
            "handoff_one_aggregate_private_bytes_model":t*vss["n"]*256,
            "handoff_one_aggregate_public_coeff_bytes_model":t*t*128,
            "baseline_49_hourly_handoffs_private_bytes_with_per_record_late_grouping_and_uniform_arrival":24.5*count*t*vss["n"]*256
        })
out["seal_bfv_8192"]={
    "ciphertext_bytes":s["profiles"][1]["ciphertext_default_save_bytes"],
    "billion_input_ciphertexts_bytes":s["profiles"][1]["ciphertext_default_save_bytes"]*1_000_000_000,
    "raw_add_single_thread_per_second":1000/s["profiles"][1]["add_one_ciphertext_ms"],
    "billion_repeat_exact":s["profiles"][1]["billion_repeat_exact"]
}
out["arithmetic_only"]={x["operation"]:{
    "single_thread_verify_per_second":1000/x["verify"]["median_ms"],
    "sequential_verify_256_ms":256*x["verify"]["median_ms"]
} for x in [issue,claim,withdraw]}
out["unmeasured_required_costs"]=[
    "source/offer authentication proof", "PayNote private spend proof",
    "Fidelity state proof", "qualified-set and availability certificates",
    "strict-only-S uint256 aggregate opening without limb-sum disclosure",
    "network/DA/storage/consensus and actual OCOMP worker",
    "chain-wide parallel contention"
]
(base/"scale_results.json").write_text(json.dumps(out,ensure_ascii=False,indent=2)+"\n")

