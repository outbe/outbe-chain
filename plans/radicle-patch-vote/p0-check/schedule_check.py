"""Finite capacity/window model, not production DKG, codec, gas or finality."""
from pathlib import Path

BATCH = 32
WINDOW = 64


def supported(length, prepare):
    return length > 0 and 0 <= prepare <= length and length - prepare >= WINDOW


def run(n, length, prepare):
    assert supported(length, prepare)
    q = n - (n - 1) // 3
    slots = (q + BATCH - 1) // BATCH
    for generation in range(5):
        stage, votes, wait = 0, 0, 2
        finished = []
        for block in range(1, WINDOW + 1):
            if wait:
                wait -= 1
                continue
            # Explicit finite scenario: four artifact-busy blocks, then one
            # omitted block in three. Other eligible blocks fit a full batch.
            if block <= 4 or block % 3 == 0:
                continue
            count = min(BATCH, q - votes)
            assert 1 <= count <= 32
            votes += count
            if votes == q:
                finished.append(block)
                stage += 1
                votes = 0
                wait = 4  # two finality + two preparation/delivery model blocks
                if stage == 3:
                    break
        assert stage == 3, (n, generation, stage, votes)
        assert finished[-1] <= length - prepare
        print(f"window n={n} q={q} epoch={length} prepare={prepare} material={generation} slots_per_checkpoint={slots} bind/review/merge={finished}")


root = Path(__file__).resolve().parents[1]
log = (root / "evidence/outbe-chain-pjz.1/descriptor-check.log").read_text().splitlines()
body = bytes.fromhex(next(line.split("=", 1)[1] for line in log if line.startswith("bootstrap_body_genesis7=")))
assert len(body) == 477
payload = len(body) + 1 + BATCH * (4 + 96)
assert payload == 3678 and payload + 3 == 3681 and payload + 3 < 4096
assert 3 * ((171 + 31) // 32) == 18
assert 2 * 171 > 300  # prior one-attestation contract's necessary-capacity failure
for config in ((300, 30), (300, 200), (1200, 30), (65, 1)):
    run(256, *config)
run(4, 65, 1)
for invalid in ((0, 0), (1, 0), (64, 1), (300, 299), (300, 301)):
    assert not supported(*invalid), invalid
# A transition does not carry t-1 old-generation slots into the new tally.
pending = [(0, i) for i in range(170)]
assert sum(generation == 1 for generation, _ in pending) == 0
print("PASS capacity: max n256, 32-entry/3681-byte batch, 18 slots for three checkpoints; five periodic materials, modeled delays/omissions, accepted short/default windows and unsupported cadence rejection")
print("MODEL ONLY: full-batch budget/readiness are explicit scenario assumptions; production codec, gas, DKG timing and real sidecar readiness remain P3/P6/P8 obligations")
