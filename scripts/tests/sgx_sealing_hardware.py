#!/usr/bin/env python3
"""Isolated hardware probe. Run inside a fresh private directory containing `probe`.

Requires Gramine, OpenSSL and SGX device access. Only public fixture data is sealed.
Run under external CPU/memory/time limits on a shared host. No node data is used.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys


ROOT = Path.cwd().resolve()
EVIDENCE = ROOT / ("cross-host-evidence.json" if sys.argv[1:] == ["cross-host"] else "evidence.json")
os.umask(0o077)
RESULT = {"host": os.uname().nodename, "binary_sha256": hashlib.sha256(
    (ROOT / "probe").read_bytes()).hexdigest(), "cases": [], "status": "running"}


def run(argv, required=True):
    p = subprocess.run(argv, text=True, stdout=subprocess.PIPE,
                       stderr=subprocess.STDOUT, timeout=90, check=False)
    if required and p.returncode:
        raise RuntimeError(f"{argv}: exit {p.returncode}\n{p.stdout}")
    return p


def identity():
    from graminelibos import Sigstruct
    sig = Sigstruct.from_bytes((ROOT / "probe.sig").read_bytes())
    return {"mrenclave": sig["enclave_hash"].hex(),
            "mrsigner": hashlib.sha256(sig["modulus"]).hexdigest()}


def sign(key):
    run(["gramine-sgx-sign", "--manifest", "probe.manifest", "--output",
         "probe.manifest.sgx", "--key", f"{key}.pem"])
    return identity()


def case(name, action, filename, expected, expected_identity):
    p = run(["gramine-sgx", "probe", action, str(ROOT / filename)], required=False)
    entry = {"case": name, "exit": p.returncode, "output": p.stdout,
             "identity": expected_identity}
    RESULT["cases"].append(entry)
    EVIDENCE.write_text(json.dumps(RESULT, indent=2) + "\n")
    assert p.returncode == expected, entry
    for label, field in [("MRENCLAVE", "mrenclave"), ("MRSIGNER", "mrsigner")]:
        assert f"{label}={expected_identity[field]}" in p.stdout, entry
    if expected == 0:
        assert "SGX_COMBINED_SEAL_PROBE_OK" in p.stdout, entry
    else:
        # Reject specifically at AEAD authentication, not a launch or EGETKEY error.
        assert "SGX_COMBINED_SEAL_PROBE_REJECTED" in p.stdout, entry
        assert "sealed blob unseal failed (bad sealing key or tampered blob)" in p.stdout, entry
    print(f"PASS {name}", flush=True)


TEMPLATE = '''
loader.entrypoint.uri = "file:{{ gramine.libos }}"
libos.entrypoint = "ROOT/probe"
loader.log_level = "error"
loader.insecure__use_cmdline_argv = true
loader.env.LD_LIBRARY_PATH = "/lib/x86_64-linux-gnu:/usr/lib/x86_64-linux-gnu"
fs.mounts = [
 {path="/lib/x86_64-linux-gnu",uri="file:/lib/x86_64-linux-gnu"},
 {path="/usr/lib/x86_64-linux-gnu",uri="file:/usr/lib/x86_64-linux-gnu"},
 {path="ROOT",uri="file:ROOT"}
]
sgx.enclave_size = "256M"
sgx.max_threads = 4
sgx.debug = false
sgx.remote_attestation = "MODE"
sgx.isvprodid = 1
sgx.isvsvn = 1
sgx.trusted_files = ["file:{{ gramine.libos }}", "file:ROOT/probe",
 "file:/lib/x86_64-linux-gnu/libc.so.6", "file:/lib/x86_64-linux-gnu/libgcc_s.so.1",
 "file:/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2"]
sgx.allowed_files = ["file:ROOT/public/"]
'''.replace("ROOT", str(ROOT))


def suite():
    (ROOT / "public").mkdir(exist_ok=False)
    for key in ["a", "b"]:
        assert not (ROOT / f"{key}.pem").exists(), "use a fresh directory"
        run(["openssl", "genrsa", "-3", "-out", f"{key}.pem", "3072"])
    for mode in ["none", "dcap"]:
        (ROOT / "probe.manifest.template").write_text(TEMPLATE.replace("MODE", mode))
        run(["gramine-manifest", "probe.manifest.template", "probe.manifest"])
        original = (ROOT / "probe.manifest").read_text()
        baseline = sign("a")
        blob = f"public/{mode}.sealed"
        case(f"{mode}/seal", "seal", blob, 0, baseline)
        data = (ROOT / blob).read_bytes()
        assert data[:5] == b"TSGX1" and int.from_bytes(data[7:9], "little") == 3
        case(f"{mode}/restart_unseal", "unseal", blob, 0, baseline)
        legacy = f"public/{mode}.legacy"
        case(f"{mode}/legacy_seal", "legacy-seal", legacy, 0, baseline)
        assert (ROOT / legacy).read_bytes()[:5] == b"TSEAL"
        case(f"{mode}/legacy_restart_unseal", "unseal", legacy, 0, baseline)
        changed_signer = sign("b")
        assert changed_signer["mrenclave"] == baseline["mrenclave"]
        assert changed_signer["mrsigner"] != baseline["mrsigner"]
        case(f"{mode}/different_signer_rejected", "unseal", blob, 6, changed_signer)
        case(f"{mode}/legacy_different_signer_rejected", "unseal", legacy, 6, changed_signer)
        case(f"{mode}/different_signer_own_seal", "seal", "public/other.sealed", 0, changed_signer)
        case(f"{mode}/different_signer_own_unseal", "unseal", "public/other.sealed", 0, changed_signer)
        changed = original.replace("[loader.env]", '[loader.env]\nOUTBE_PROBE_MEASUREMENT = "changed"')
        assert changed != original
        (ROOT / "probe.manifest").write_text(changed)
        changed_measurement = sign("a")
        assert changed_measurement["mrenclave"] != baseline["mrenclave"]
        assert changed_measurement["mrsigner"] == baseline["mrsigner"]
        case(f"{mode}/different_measurement_rejected", "unseal", blob, 6, changed_measurement)
        case(f"{mode}/different_measurement_own_seal", "seal", "public/other.sealed", 0, changed_measurement)
        case(f"{mode}/different_measurement_own_unseal", "unseal", "public/other.sealed", 0, changed_measurement)
        case(f"{mode}/legacy_new_measurement_unseal", "unseal", legacy, 0, changed_measurement)
        case(f"{mode}/legacy_reseal_combined", "reseal", legacy, 0, changed_measurement)
        migrated = (ROOT / legacy).read_bytes()
        assert migrated[:5] == b"TSGX1" and int.from_bytes(migrated[7:9], "little") == 3
        case(f"{mode}/legacy_combined_restart_unseal", "unseal", legacy, 0, changed_measurement)
        (ROOT / "probe.manifest").write_text(original)
        assert sign("a") == baseline
        case(f"{mode}/restored_identity_unseal", "unseal", blob, 0, baseline)
        case(f"{mode}/legacy_migrated_old_measurement_rejected", "unseal", legacy, 6, baseline)
        if mode == "dcap":
            case("dcap/quote", "quote", "public/quote.bin", 0, baseline)
            quote = (ROOT / "public/quote.bin").read_bytes()
            assert int.from_bytes(quote[:2], "little") == 3
            assert quote[112:144].hex() == baseline["mrenclave"]
            assert quote[176:208].hex() == baseline["mrsigner"]
            assert quote[368:432] == bytes([0x73]) * 64
            RESULT["quote_sha256"] = hashlib.sha256(quote).hexdigest()
            RESULT["quote_bytes"] = len(quote)


if __name__ == "__main__":
    try:
        if sys.argv[1:] == ["cross-host"]:
            case("different_host_rejected", "unseal", "public/dcap.sealed", 6, identity())
            case("different_host_own_seal", "seal", "public/cross.sealed", 0, identity())
            case("different_host_own_unseal", "unseal", "public/cross.sealed", 0, identity())
        else:
            assert not sys.argv[1:]
            suite()
        RESULT["status"] = "passed"
    except Exception as error:
        RESULT["status"] = "failed"
        RESULT["error"] = str(error)
        raise
    finally:
        EVIDENCE.write_text(json.dumps(RESULT, indent=2) + "\n")
