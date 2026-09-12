#!/usr/bin/env python3
"""Three passive Shamir actors; each reads ONLY its own existing VSS shares.

Public timestamps/padded cohort slots are an explicit PoC profile. Only league,
validity and declared aggregate outputs are reconstructed. New balances/payouts
are exported as local shares, never opened to the controller.
"""
import json
import os
from pathlib import Path
import resource
import secrets
import sys
import time
from mpyc.runtime import mpc

Q = 7237005577332262213973186563042994240857116359379907606001950938285454250989
F = mpc.SecFld(order=Q, signed=False)
W = mpc.SecInt(512)
K = 10**18
MAX = 2**256

def write(path, value, private=False):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600 if private else 0o644)
    with os.fdopen(fd, "w") as f:
        json.dump(value, f)
        f.flush()
        os.fsync(f.fileno())

def import_q(y):
    y = int(y)
    if not 0 <= y < Q:
        raise ValueError("noncanonical VSS share")
    v = F()
    v.set_share(F.field(y))
    return v

def secret_div(a, b, bits):
    # Exact restoring division; no SecFxp or field-inverse-as-floor.
    q, r = W(0), W(0)
    for bit in reversed(mpc.to_bits(a, l=bits)):
        r = 2*r + bit
        take = r >= b
        r -= take*b
        q = 2*q + take
    return q, r

async def main():
    plan = json.loads(Path(sys.argv[1]).read_text())
    me = mpc.pid
    private = Path(plan["members"][me])
    store = json.loads((private / "shares.private.json").read_text())
    assert store["epoch"] == plan["epoch"]
    records = store["records"]
    for row in records.values():
        assert row["share"]["x"] == me + 1
    # The native acceptance/reshare path already verified the VSS equations.
    # Passive actors import those exact durable bytes; active binding is NOT
    # a guarantee of MPyC and is reported as a production gate.
    cache = {}
    def amount(name, limbs=True):
        if name not in cache:
            ids = [f"{name}:{k}" for k in range(4)] if limbs else [name]
            xs = mpc.convert([import_q(records[x]["share"]["y"]) for x in ids], W)
            cache[name] = sum((x * 2**(64*k) for k, x in enumerate(xs)), W(0))
        return cache[name]

    await mpc.start()
    refs = {p.pid: p.protocol for p in mpc.parties if p.pid != me}
    started = time.perf_counter()
    result = {"security": "passive n=3 degree=1; localhost", "epoch": plan["epoch"], "context": plan["context"], "operation": plan["op"]}
    exported = {}

    async def export(name, value):
        valid = (value >= 0) * (value < MAX)
        if not await mpc.output(valid):
            raise ValueError("private output uint256 range failed")
        bs = mpc.to_bits(value, l=256)
        limbs = [mpc.from_bits(bs[i:i+64]) for i in range(0, 256, 64)]
        local_values = await mpc.gather(mpc.convert(limbs, F))
        rows = []
        for limb in local_values:
            # Sum independently random inputs. No one knows the resulting blind.
            r = mpc.sum(mpc.input(F(secrets.randbelow(Q))))
            local_r = await mpc.gather(r)
            rows.append({"x": me+1, "y": str(int(limb.value)), "z": str(int(local_r.value))})
        exported[name] = rows

    if plan["op"] == "update":
        result["request"] = plan["request"]
        active = [amount(row["id"]) for row in plan["active"]]
        before = mpc.sum(active) if active else W(0)
        expected_before = sum((amount(x) for x in plan["old_balance"]), W(0))
        expected_after = sum((amount(x) for x in plan["new_balance"]), W(0))
        delta = plan["delta"]
        if "public" in delta:
            change = W(int(delta["public"]))
        else:
            change = amount(delta["id"], limbs=delta.get("limbs", True))*int(delta.get("factor", "1"))
        valid = (before == expected_before)*(change >= 0)*(change < MAX)
        outputs, sold = list(active), []
        if plan["direction"] == "in":
            outputs.append(change)
        elif plan["direction"] == "out":
            remaining = change
            sold = [W(0) for _ in active]
            valid *= (remaining <= before)
            for i in reversed(range(len(active))):
                take = mpc.if_else(remaining < active[i], remaining, active[i])
                outputs[i] = active[i]-take
                sold[i] = take
                remaining -= take
            valid *= (remaining == 0)
        after = mpc.sum(outputs) if outputs else W(0)
        valid *= (after == expected_after)*(after < MAX)
        a = sum((value*int(row["decay"]) for value,row in zip(outputs,plan["next_active"])), W(0))
        d = a + sum((amount(row["id"])*int(row["held"]) for row in plan["sold"]),W(0))
        d += sum((value*int(row["held"]) for value,row in zip(sold,plan["new_sold"])), W(0))
        valid *= (a < MAX)*(d < MAX)*((d == 0)+(d != 0)*(a*K < MAX))
        if not await mpc.output(valid):
            raise ValueError("money/cohort conservation or checked pre-write evaluation")
        for i,value in enumerate(outputs):
            await export(f"active-{i}",value)
        for i,value in enumerate(sold):
            await export(f"sold-{i}",value)
        result["money_cohort_conservation"] = True
        result["checked_u256_valid"] = True
        result["transition_layout"] = {
            "active": [{"id": f"{plan['tag']}-active-{i}", "at": row["at"]} for i,row in enumerate(plan["next_active"])],
            "sold": [{"id": row["id"], "at": row["at"], "sold_at": row["sold_at"]} for row in plan["sold"]] + [{"id": f"{plan['tag']}-sold-{i}", "at": row["at"], "sold_at": row["sold_at"]} for i,row in enumerate(plan["new_sold"])],
            "qualified": plan["qualified_after"], "version": plan["context"]["cohort_version"]+1,
            "output_ids": [f"{plan['tag']}-active-{i}" for i in range(len(outputs))] + [f"{plan['tag']}-sold-{i}" for i in range(len(sold))],
            "preserved_sold": [{k:v for k,v in row.items() if k!="held"} for row in plan["sold"]],
        }
    elif plan["op"] == "fidelity":
        accounts = plan["accounts"]
        numerators, denominators, validities = [], [], []
        for account in accounts:
            active = sum((amount(r["id"])*int(r["decay"]) for r in account["active"]), W(0))
            sold = sum((amount(r["id"])*int(r["held"]) for r in account["sold"]), W(0))
            denominator = active + sold
            valid = (active < MAX) * (denominator < MAX)
            if account["qualified"]:
                valid *= ((denominator == 0) + (denominator != 0)*(active*K < MAX))
            numerators.append(active*K)
            denominators.append(denominator)
            validities.append(valid)
        valid = await mpc.output(mpc.prod(validities))
        result["checked_u256_valid"] = bool(valid)
        if not valid:
            raise ValueError("source Fidelity checked intermediate overflow")
        lows, highs = [0]*len(accounts), [4095]*len(accounts)
        # Every revealed bit is a deterministic function of the final public
        # league. Guards ensure a zero denominator/age follows the same rule.
        for _ in range(12):
            decisions = []
            for i, a in enumerate(accounts):
                mid = (lows[i]+highs[i]+1)//2
                z, w = int(a["age"]), int(a["maximum"])
                if not a["qualified"] or z == 0 or w == 0:
                    decisions.append(W(0))
                else:
                    h = (mid*w+4095)//4096
                    threshold = (h*K+z-1)//z
                    decisions.append((denominators[i] > 0) * (numerators[i] >= threshold*denominators[i]))
            directions = await mpc.output(decisions)
            for i, bit in enumerate(directions):
                mid = (lows[i]+highs[i]+1)//2
                if bit:
                    lows[i] = mid
                else:
                    highs[i] = mid-1
        result["leagues"] = [x+1 for x in lows]
        result["public_comparison_rounds"] = 12

    elif plan["op"] == "forced-out":
        # Active pledge authority/context is checked by the public coordinator
        # before invoking this T2a attempt and again before T2b finality.
        remaining = W(int(plan["debit18"]))
        active = [amount(row["id"]) for row in plan["active"]]
        valid = (remaining >= 0) * (remaining <= mpc.sum(active))
        outputs, sold = list(active), [W(0) for _ in active]
        for i in reversed(range(len(active))):
            take = mpc.if_else(remaining < active[i], remaining, active[i])
            outputs[i] = active[i]-take
            sold[i] = take
            remaining -= take
        valid *= (remaining == 0)
        if not await mpc.output(valid):
            raise ValueError("insufficient private eligible state")
        # Checked pre-write Fidelity evaluation at EXACT candidate timestamp.
        a = mpc.sum([x*int(row["decay"]) for x, row in zip(outputs, plan["active"])])
        d = a + mpc.sum([x*int(row["held_after_sale"]) for x, row in zip(sold, plan["active"])])
        checked = (a < MAX)*(d < MAX)*((d == 0)+(d != 0)*(a*K < MAX))
        if not await mpc.output(checked):
            raise ValueError("forced-out Fidelity evaluation failed before commit")
        for i, (value, consumed) in enumerate(zip(outputs, sold)):
            await export(f"active-{i}", value)
            await export(f"sold-{i}", consumed)
        result["checked_u256_valid"] = True
        result["padded_output_slots"] = len(outputs)*2

    elif plan["op"] == "expiry":
        returned = sum((amount(row["id"],False)*int(row["factor"]) for row in plan["rights"]),W(0))
        if not await mpc.output((returned >= 0)*(returned <= int(plan["reserved18"]))):
            raise ValueError("expiry exceeds reserved budget")
        await export("returned-limit",returned)
        result["unclaimed_set_bound_to_context"] = True
        result["private_limit_return"] = True
        result["checked_u256_valid"] = True
    elif plan["op"] == "intex":
        ns = [amount(name, False) for name in plan["rights"]]
        denominator = mpc.sum(ns)
        if not await mpc.output(denominator > 0):
            raise ValueError("empty eligible payout set")
        payouts = []
        pool = int(plan["pool18"])
        if not 0 < pool < MAX:
            raise ValueError("pool uint256")
        for i, nominal in enumerate(ns):
            value, _ = secret_div(nominal*pool, denominator, 104+pool.bit_length())
            payouts.append(value)
            await export(f"payout-{i}", value)
        # Last-leaf carry is deliberately not used in the experimental profile.
        await export("round-remainder", W(pool)-mpc.sum(payouts))
        result["independent_floor"] = True
        result["private_outputs"] = len(payouts)+1
        result["checked_u256_valid"] = True
    else:
        raise ValueError("unknown private computation")

    elapsed = time.perf_counter()-started
    for name, shares in exported.items():
        write(private / f"{plan['tag']}-{name}.private.json", shares, private=True)
    await mpc.shutdown()
    usage = resource.getrusage(resource.RUSAGE_SELF)
    result.update(pid=me, elapsed_seconds=elapsed, cpu_seconds=usage.ru_utime+usage.ru_stime,
                  peak_rss_bytes=usage.ru_maxrss if sys.platform == "darwin" else usage.ru_maxrss*1024,
                  framed_sent={str(pid): peer.nbytes_sent for pid, peer in refs.items()})
    write(Path(plan["out"]) / f"party-{me}.json", result)

mpc.run(main())
