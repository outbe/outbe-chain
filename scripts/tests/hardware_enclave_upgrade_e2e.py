#!/usr/bin/env python3
"""Local SGX migration with release ELFs and a disposable genesis.

Run with --scenario migration and OUTBE_OLD_TEST_ENCLAVE=/path/to/old/enclave.
Fresh manifests measure the test genesis; this does not reuse a live-node seal.
"""
import hashlib
import os
from pathlib import Path

import enclave_upgrade_e2e as module

MANIFEST = """
loader.entrypoint.uri = "file:{{ gramine.libos }}"
libos.entrypoint = "BINARY"
loader.log_level = "error"
loader.insecure__use_cmdline_argv = true
loader.env.LD_LIBRARY_PATH = "/lib:/lib/x86_64-linux-gnu:/usr/lib/x86_64-linux-gnu"
loader.env.MALLOC_ARENA_MAX = "1"
sys.enable_sigterm_injection = true
fs.mounts = [
 {path="/lib",uri="file:{{ gramine.runtimedir() }}"},
 {path="/lib/x86_64-linux-gnu",uri="file:/lib/x86_64-linux-gnu"},
 {path="/usr/lib/x86_64-linux-gnu",uri="file:/usr/lib/x86_64-linux-gnu"},
 {path="ROOT",uri="file:ROOT"},
 {path="BINARY",uri="file:BINARY"},
 {path="/opt/outbe/sgx/network-descriptor-v1.bin",uri="file:DESCRIPTOR"}
]
sgx.enclave_size = "256M"
sgx.max_threads = 16
sgx.debug = false
sgx.remote_attestation = "none"
sgx.isvprodid = 1
sgx.isvsvn = 1
sgx.trusted_files = ["file:{{ gramine.libos }}", "file:BINARY", "file:DESCRIPTOR",
 "file:{{ gramine.runtimedir() }}/", "file:/lib/x86_64-linux-gnu/libc.so.6",
 "file:/lib/x86_64-linux-gnu/libgcc_s.so.1", "file:/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2"]
sgx.allowed_files = ["file:STATE/"]
"""


class HardwareNetwork(module.Network):
    combined_seal_magic = b"TSGX1"

    def prepare(self):
        if self.scenario != "migration":
            raise ValueError("hardware runner requires --scenario migration")
        self.old_enclave = Path(os.environ["OUTBE_OLD_TEST_ENCLAVE"]).resolve(strict=True)
        super().prepare()
        self.report.update(hardware_fixture_manifests=True, hardware_sgx=True)
        for key in ["a", "b"]:
            self.run(["openssl", "genrsa", "-3", "-out", self.directory / f"signer-{key}.pem", "3072"],
                     f"signer-{key}")
        self.report["old_enclave_sha256"] = hashlib.sha256(self.old_enclave.read_bytes()).hexdigest()

    def start_enclave(self, i, candidate=False, combined=False):
        name = f"enclave-{'candidate-' if candidate else ''}{i}"
        directory = self.node_dir(i) / ("candidate-tee" if candidate else "tee")
        directory.mkdir(parents=True, exist_ok=True)
        binary = self.args.enclave if candidate else self.old_enclave
        prefix = self.directory / name
        if not prefix.with_suffix(".manifest.sgx").exists():
            text = (MANIFEST.replace("ROOT", str(self.directory)).replace("BINARY", str(binary))
                    .replace("DESCRIPTOR", str(self.network / "network-descriptor.bin"))
                    .replace("STATE", str(directory)))
            template = prefix.with_suffix(".manifest.template")
            template.write_text(text)
            self.run(["gramine-manifest", template, prefix.with_suffix(".manifest")], name + "-manifest")
            key = "b" if combined else "a"
            self.run(["gramine-sgx-sign", "--manifest", prefix.with_suffix(".manifest"),
                      "--output", prefix.with_suffix(".manifest.sgx"),
                      "--key", self.directory / f"signer-{key}.pem"], name + "-sign")
            from graminelibos import Sigstruct
            sig = Sigstruct.from_bytes(prefix.with_suffix(".sig").read_bytes())
            self.report.setdefault("hardware_fixture_identities", {})[name] = {
                "mrenclave": sig["enclave_hash"].hex(),
                "mrsigner": hashlib.sha256(sig["modulus"]).hexdigest(),
            }
        self.start(name, ["gramine-sgx", prefix, "--socket",
                         f"127.0.0.1:{self.port('enclave', i + (100 if candidate else 0))}",
                         "--tee-dir", directory, "--chain-id", f"{424242:064x}"])


if __name__ == "__main__":
    module.Network = HardwareNetwork
    module.main()
