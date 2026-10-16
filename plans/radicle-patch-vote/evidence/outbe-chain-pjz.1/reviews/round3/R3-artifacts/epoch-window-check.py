"""Necessary carrier-capacity model, not production DKG/execution."""
n = 256
q = n - (n - 1) // 3
window = 300
assert q == 171
assert 2 * q == 342 > window
# Ideal delivery, every block available, no network/finality/Git delays.
# Each numeric material change clears unfinished current-generation slots;
# the next current-material binding precedes owner publication.
reviews = 0
for epoch in range(1, 6):
    binding = review = 0
    for block in range(window):
        if binding < q:
            binding += 1
        elif review < q:
            review += 1
            if review == q:
                reviews += 1
    assert binding == q and review == window - q == 129
    print(f"epoch={epoch} binding={binding}/{q} review={review}/{q} reset_pending=True")
assert reviews == 0
print("COUNTEREXAMPLE: stable membership, timely material rotations, perfect delivery; one attestation/block cannot complete BindMaterial+Review before reset")
