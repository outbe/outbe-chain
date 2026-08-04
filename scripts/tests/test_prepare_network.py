#!/usr/bin/env python3

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
PREPARE_NETWORK = REPO_ROOT / "scripts" / "prepare_network.py"
SEED = REPO_ROOT / "scripts" / "seed-testnet-lowstake.json"


def project_binary(name: str) -> Path:
    override = os.environ.get(f"OUTBE_{name.upper().replace('-', '_')}_BINARY")
    candidates = [
        Path(override) if override else None,
        REPO_ROOT / "target" / "debug" / name,
        REPO_ROOT / "target" / "release" / name,
    ]
    for candidate in candidates:
        if candidate is not None and candidate.is_file() and os.access(candidate, os.X_OK):
            return candidate
    raise unittest.SkipTest(f"{name} is not built")


class PrepareNetworkTests(unittest.TestCase):
    def test_existing_founders_accept_matching_private_identities(self) -> None:
        chain = project_binary("outbe-chain")
        keygen = project_binary("outbe-keygen")

        with tempfile.TemporaryDirectory(prefix="outbe-prepare-network-") as temporary:
            root = Path(temporary)
            material = root / "founders"
            output = root / "network"
            subprocess.run(
                [
                    str(chain),
                    "dkg",
                    "identities",
                    "--output-dir",
                    str(material),
                    "--validators",
                    "4",
                ],
                cwd=REPO_ROOT,
                check=True,
            )

            subprocess.run(
                [
                    "python3",
                    str(PREPARE_NETWORK),
                    "--seed",
                    str(SEED),
                    "--validators",
                    str(material / "validators.json"),
                    "--founder-material-dir",
                    str(material),
                    "--output-dir",
                    str(output),
                    "--chain-binary",
                    str(chain),
                    "--keygen-binary",
                    str(keygen),
                    "--tee-mode",
                    "gramine-direct-dev",
                    "--enclave-image",
                    "outbe-tee-enclave-gramine-test:local",
                    "--use-local-defaults",
                ],
                cwd=REPO_ROOT,
                check=True,
            )

            self.assertTrue((output / "genesis.json").is_file())
            for index in range(4):
                self.assertTrue(
                    (output / "commands" / f"validator-{index}.sh").is_file()
                )
                self.assertTrue(
                    (output / "commands" / f"enclave-{index}.sh").is_file()
                )

    def test_existing_founders_reject_mismatched_bls_identity(self) -> None:
        chain = project_binary("outbe-chain")
        keygen = project_binary("outbe-keygen")

        with tempfile.TemporaryDirectory(prefix="outbe-prepare-network-") as temporary:
            root = Path(temporary)
            material = root / "founders"
            output = root / "network"
            subprocess.run(
                [
                    str(chain),
                    "dkg",
                    "identities",
                    "--output-dir",
                    str(material),
                    "--validators",
                    "4",
                ],
                cwd=REPO_ROOT,
                check=True,
            )
            (material / "validator-0" / "signing-key.hex").write_bytes(
                (material / "validator-1" / "signing-key.hex").read_bytes()
            )

            result = subprocess.run(
                [
                    "python3",
                    str(PREPARE_NETWORK),
                    "--seed",
                    str(SEED),
                    "--validators",
                    str(material / "validators.json"),
                    "--founder-material-dir",
                    str(material),
                    "--output-dir",
                    str(output),
                    "--chain-binary",
                    str(chain),
                    "--keygen-binary",
                    str(keygen),
                    "--tee-mode",
                    "gramine-direct-dev",
                    "--enclave-image",
                    "outbe-tee-enclave-gramine-test:local",
                    "--use-local-defaults",
                ],
                cwd=REPO_ROOT,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn(
                "validator-0 BLS signing key does not match validators.json",
                result.stderr,
            )
            self.assertFalse((output / "genesis.json").exists())
            self.assertFalse((output / "commands").exists())

    def test_existing_founders_reject_mismatched_evm_identity(self) -> None:
        chain = project_binary("outbe-chain")
        keygen = project_binary("outbe-keygen")

        with tempfile.TemporaryDirectory(prefix="outbe-prepare-network-") as temporary:
            root = Path(temporary)
            material = root / "founders"
            output = root / "network"
            subprocess.run(
                [
                    str(chain),
                    "dkg",
                    "identities",
                    "--output-dir",
                    str(material),
                    "--validators",
                    "4",
                ],
                cwd=REPO_ROOT,
                check=True,
            )
            (material / "validator-0" / "evm-key.hex").write_bytes(
                (material / "validator-1" / "evm-key.hex").read_bytes()
            )

            result = subprocess.run(
                [
                    "python3",
                    str(PREPARE_NETWORK),
                    "--seed",
                    str(SEED),
                    "--validators",
                    str(material / "validators.json"),
                    "--founder-material-dir",
                    str(material),
                    "--output-dir",
                    str(output),
                    "--chain-binary",
                    str(chain),
                    "--keygen-binary",
                    str(keygen),
                    "--tee-mode",
                    "gramine-direct-dev",
                    "--enclave-image",
                    "outbe-tee-enclave-gramine-test:local",
                    "--use-local-defaults",
                ],
                cwd=REPO_ROOT,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn(
                "validator-0 EVM signing key does not match validators.json",
                result.stderr,
            )
            self.assertFalse((output / "genesis.json").exists())
            self.assertFalse((output / "commands").exists())

    def test_dcap_network_rejects_mutable_enclave_image_tag(self) -> None:
        chain = project_binary("outbe-chain")
        keygen = project_binary("outbe-keygen")
        with tempfile.TemporaryDirectory(prefix="outbe-prepare-network-") as temporary:
            result = subprocess.run(
                [
                    "python3",
                    str(PREPARE_NETWORK),
                    "--seed",
                    str(SEED),
                    "--generate-validators",
                    "4",
                    "--validator-hosts",
                    "127.0.0.1",
                    "--output-dir",
                    str(Path(temporary) / "network"),
                    "--chain-binary",
                    str(chain),
                    "--keygen-binary",
                    str(keygen),
                    "--tee-mode",
                    "dcap-required",
                    "--chain-id",
                    "54322345",
                    "--mrenclave",
                    "0x" + "11" * 32,
                    "--mrsigner",
                    "0x" + "22" * 32,
                    "--isv-prod-id",
                    "1",
                    "--minimum-isv-svn",
                    "1",
                    "--minimum-tcb-evaluation-data-number",
                    "1",
                    "--enclave-image",
                    "ghcr.io/outbe/outbe-tee-enclave:testnet",
                ],
                cwd=REPO_ROOT,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("immutable image digest", result.stderr)

    def test_generated_dcap_testnet_bundle_is_bound_to_release_and_sgx_devices(self) -> None:
        chain = project_binary("outbe-chain")
        keygen = project_binary("outbe-keygen")
        image = "ghcr.io/outbe/outbe-tee-enclave@sha256:" + "aa" * 32
        with tempfile.TemporaryDirectory(prefix="outbe-prepare-network-") as temporary:
            output = Path(temporary) / "network"
            subprocess.run(
                [
                    "python3",
                    str(PREPARE_NETWORK),
                    "--seed",
                    str(SEED),
                    "--generate-validators",
                    "4",
                    "--validator-hosts",
                    "127.0.0.1",
                    "--output-dir",
                    str(output),
                    "--runtime-base-dir",
                    "/var/lib/outbe/testnet",
                    "--chain-binary",
                    str(chain),
                    "--keygen-binary",
                    str(keygen),
                    "--runtime-chain-binary",
                    "/usr/local/bin/outbe-chain",
                    "--tee-mode",
                    "dcap-required",
                    "--chain-id",
                    "54322345",
                    "--mrenclave",
                    "0x" + "11" * 32,
                    "--mrsigner",
                    "0x" + "22" * 32,
                    "--isv-prod-id",
                    "1",
                    "--minimum-isv-svn",
                    "1",
                    "--minimum-tcb-evaluation-data-number",
                    "1",
                    "--enclave-image",
                    image,
                ],
                cwd=REPO_ROOT,
                check=True,
            )

            genesis = json.loads((output / "genesis.json").read_text())
            self.assertEqual(genesis["config"]["chainId"], 54322345)
            tee_manifest = json.dumps(genesis["config"]["teeAttestationV1"])
            self.assertIn("11" * 32, tee_manifest)
            self.assertIn("22" * 32, tee_manifest)
            self.assertFalse((output / "test-sgx-signing-key.pem").exists())
            for index in range(4):
                enclave = (output / "commands" / f"enclave-{index}.sh").read_text()
                self.assertIn(image, enclave)
                self.assertIn("--device /dev/sgx_enclave:/dev/sgx_enclave", enclave)
                self.assertIn("--device /dev/sgx_provision:/dev/sgx_provision", enclave)
                self.assertNotIn("--dkg-seed", enclave)
                subprocess.run(
                    ["bash", "-n", str(output / "commands" / f"enclave-{index}.sh")],
                    check=True,
                )

    def test_generated_devnet_bundle_contains_every_founder_startup_input(self) -> None:
        chain = project_binary("outbe-chain")
        keygen = project_binary("outbe-keygen")

        with tempfile.TemporaryDirectory(prefix="outbe-prepare-network-") as temporary:
            output = Path(temporary) / "network"
            runtime = "/var/lib/outbe/devnet"
            subprocess.run(
                [
                    "python3",
                    str(PREPARE_NETWORK),
                    "--seed",
                    str(SEED),
                    "--generate-validators",
                    "4",
                    "--validator-hosts",
                    "127.0.0.1",
                    "--output-dir",
                    str(output),
                    "--runtime-base-dir",
                    runtime,
                    "--chain-binary",
                    str(chain),
                    "--keygen-binary",
                    str(keygen),
                    "--runtime-chain-binary",
                    "/usr/local/bin/outbe-chain",
                    "--tee-mode",
                    "gramine-direct-dev",
                    "--enclave-image",
                    "outbe-tee-enclave-gramine-test:local",
                    "--use-local-defaults",
                ],
                cwd=REPO_ROOT,
                check=True,
            )

            genesis = json.loads((output / "genesis.json").read_text())
            config = genesis["config"]
            self.assertIn("ocompForkInstallV1", config)
            self.assertIn("metadosisStorageLayoutV1", config)
            self.assertIn("teeAttestationV1", config)
            self.assertTrue((output / "protocol-bundle-v1.ocb1").is_file())
            self.assertFalse(
                (output / "polynomial.hex").exists(),
                "fresh founders must run the interactive genesis DKG, not consume a centralized bootstrap polynomial",
            )
            self.assertFalse((output / "dkg-output.hex").exists())

            validators = json.loads((output / "validators.json").read_text())
            self.assertEqual(len(validators), 4)
            self.assertEqual(len({entry["p2p_address"] for entry in validators}), 4)
            self.assertEqual(len({entry["reth_p2p_address"] for entry in validators}), 4)

            node_scripts = []
            enclave_scripts = []
            projection_scope = (output / "projection-scope").read_text().strip()
            for index in range(4):
                validator_dir = output / f"validator-{index}"
                self.assertTrue((validator_dir / "ocomp-key-v1.hex").is_file())
                self.assertTrue((validator_dir / "ocomp-registration-v1.ocb1").is_file())
                self.assertFalse((validator_dir / "signing-share.hex").exists())

                node_script = output / "commands" / f"validator-{index}.sh"
                enclave_script = output / "commands" / f"enclave-{index}.sh"
                node_scripts.append(node_script.read_text())
                enclave_scripts.append(enclave_script.read_text())
                subprocess.run(["bash", "-n", str(node_script)], check=True)
                subprocess.run(["bash", "-n", str(enclave_script)], check=True)

                node = node_scripts[-1]
                self.assertNotIn("--consensus.signing-share", node)
                self.assertNotIn("--consensus.public-polynomial", node)
                self.assertNotIn("--consensus.dkg-output", node)
                self.assertIn("--projection.mongodb-uri", node)
                self.assertIn("--engine.persistence-threshold 0", node)
                self.assertIn("--engine.memory-block-buffer-target 0", node)
                self.assertIn('export RUST_MIN_STACK="${RUST_MIN_STACK:-16777216}"', node)
                self.assertIn(
                    f"--projection.mongodb-database 'outbe_devnet_{projection_scope}_validator_{index}'",
                    node,
                )
                self.assertTrue(node.startswith("#!/usr/bin/env bash\nset -euo pipefail\n"))
                self.assertIn("/usr/local/bin/outbe-chain node", node)
                self.assertIn(
                    f"--name 'outbe-{projection_scope}-enclave-{index}'",
                    enclave_scripts[-1],
                )

            self.assertEqual(
                len(
                    {
                        line.strip()
                        for script in node_scripts
                        for line in script.splitlines()
                        if "--tee-enclave-socket" in line
                    }
                ),
                4,
            )
            self.assertTrue(all("docker run" in script for script in enclave_scripts))


if __name__ == "__main__":
    unittest.main()
