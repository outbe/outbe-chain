#!/usr/bin/env python3
"""Render the real test manifest and verify its hardware trust boundary."""
import unittest
from pathlib import Path
from types import SimpleNamespace
import tomllib
from jinja2 import Environment, StrictUndefined

TEMPLATE = Path(__file__).resolve().parents[2] / "bin/outbe-tee-enclave/gramine/outbe-tee-enclave.manifest.template"
DESCRIPTOR = "/opt/outbe/sgx/network-descriptor-v1.bin"


class SgxTestManifest(unittest.TestCase):
    def test_descriptor_is_measured_for_both_hardware_modes(self):
        for mode, hardware in [("none", False), ("none", True), ("dcap", True)]:
            with self.subTest(mode=mode, hardware=hardware):
                rendered = Environment(undefined=StrictUndefined).from_string(TEMPLATE.read_text()).render(
                    entrypoint="/app/outbe-tee-enclave", log_level="error",
                    arch_libdir="/lib/x86_64-linux-gnu", tee_dir="/tee",
                    remote_attestation=mode, network_descriptor=DESCRIPTOR,
                    network_descriptor_enabled="1" if hardware else "0", qvl_host_dir="/qvl",
                    gramine=SimpleNamespace(libos="/gramine/libsysdb.so", runtimedir=lambda: "/gramine/runtime"))
                manifest = tomllib.loads(rendered)
                trusted = manifest["sgx"]["trusted_files"]
                mounted = {mount["path"] for mount in manifest["fs"]["mounts"]}
                self.assertEqual("file:" + DESCRIPTOR in trusted, hardware)
                self.assertEqual(DESCRIPTOR in mounted, hardware)
                self.assertNotIn("file:" + DESCRIPTOR, manifest["sgx"]["allowed_files"])
                self.assertEqual("/qvl" in mounted, mode == "dcap")
                self.assertEqual("file:/qvl/libsgx_dcap_quoteverify.so.1" in trusted, mode == "dcap")
                self.assertEqual(manifest["sgx"]["remote_attestation"], mode)


if __name__ == "__main__":
    unittest.main()
