"""Independent current production seeder byte-bound diagnostic; no bootstrap claim."""
import copy
import sys
from pathlib import Path
sys.dont_write_bytecode = True
root = Path(__file__).resolve().parents[6]
sys.path.insert(0, str(root / "scripts"))
from test_genesis_governments import genesis, configuration
from genesis_seed_alloc import seed_governments_descriptor
from genesis_seed_codec import StorageBuilder
for description in ["x" * 255, "x" * 256, "я" * 127, "я" * 127 + "x", "я" * 128, "x" * 1024, "x" * 1025]:
    config = copy.deepcopy(configuration())
    config["governments"]["description"] = description
    storage = StorageBuilder()
    try:
        seed_governments_descriptor(genesis(), storage, config)
        accepts = True
    except ValueError:
        accepts = False
    size = len(description.encode("utf-8"))
    print(f"utf8_bytes={size} seeder_accepts={accepts} seeded_entries={len(storage.entries)}")
    assert accepts == (size <= 1024)
    if not accepts:
        assert storage.entries == {}
print("CONFIRMED current production seeder allows 256..1024 bytes; not bootstrap PASS")
