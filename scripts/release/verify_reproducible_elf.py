#!/usr/bin/env python3
"""Compare two clean Outbe ELF build outputs and save canonical evidence."""

from __future__ import annotations

import argparse
import hashlib
import importlib.metadata
import importlib.util
import json
from pathlib import Path
from typing import Any

from jsonschema import Draft202012Validator

EXPECTED_ARTIFACTS = (
    "outbe-chain",
    "outbe-cli",
    "outbe-keygen",
    "outbe-feeder",
    "outbe-radicle",
    "outbe-tee-enclave",
)
FORBIDDEN_PATHS = (b"/workspace", b"/usr/local/cargo", b"/usr/local/rustup")
# This list is deliberately independent of the manifest generator's copy. The
# second-builder verifier must still reject a marker if generation was bypassed or regressed.
PRODUCTION_TEE_FORBIDDEN_MARKERS = (
    b"outbe-tee-enclave-mock: MOCK ENCLAVE",
    b"outbe-dcap-capture-enclave",
    b"OUTBE_QVL_TEST_TRACE",
    b"OUTBE_QVL_BEGIN",
    b"OUTBE_QVL_END",
    b"explicit development seed",
    b"development offer-secret fallback",
)
EXPECTED_CHECKSUM_PATHS = frozenset(
    [*(f"bin/{name}" for name in EXPECTED_ARTIFACTS)]
    + [
        "metadata/builder-system-packages.txt",
        "metadata/outbe-chain.version.txt",
        "metadata/release-manifest-v1.schema.json",
        "metadata/reproducible-elf-build-v1.json",
        "release-manifest.json",
    ]
)
PINNED_VERIFIER_DISTRIBUTIONS = {
    "attrs": "23.2.0",
    "jsonschema": "4.10.3",
    "pyrsistent": "0.20.0",
}


def _load_version_verifier():
    path = Path(__file__).with_name("verify_outbe_chain_version.py")
    spec = importlib.util.spec_from_file_location("outbe_chain_version_verifier", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load version verifier: {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


version_verifier = _load_version_verifier()


def _load_helper(name: str):
    path = Path(__file__).with_name(f"reproducible_elf_{name}.py")
    spec = importlib.util.spec_from_file_location(f"reproducible_elf_{name}", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


_materials = _load_helper("materials")
_artifacts = _load_helper("artifacts")
_identity = _load_helper("identity")


def _canonical_json(value: Any) -> bytes:
    return (
        json.dumps(
            value,
            ensure_ascii=True,
            allow_nan=False,
            sort_keys=True,
            separators=(",", ":"),
        )
        + "\n"
    ).encode("utf-8")


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _verify_checksum_file(output: Path, label: str, differences: list[str]) -> bytes:
    return _materials._verify_checksum_file(output, label, differences, EXPECTED_CHECKSUM_PATHS)


def _verify_environment(differences: list[str]) -> None:
    for distribution, expected in PINNED_VERIFIER_DISTRIBUTIONS.items():
        try:
            actual = importlib.metadata.version(distribution)
        except importlib.metadata.PackageNotFoundError:
            differences.append(f"verifier environment: missing {distribution}=={expected}")
            continue
        if actual != expected:
            differences.append(
                f"verifier environment: expected {distribution}=={expected}, found {actual}"
            )


def _read_manifest(
    output: Path,
    schema: dict[str, Any],
    label: str,
    differences: list[str],
) -> tuple[dict[str, Any] | None, bytes]:
    path = output / "release-manifest.json"
    if not path.is_file():
        differences.append(f"{label}: missing release-manifest.json")
        return None, b""
    raw = path.read_bytes()
    try:
        manifest = json.loads(raw)
        Draft202012Validator(schema).validate(manifest)
        if raw != _canonical_json(manifest):
            differences.append(f"{label}: release manifest is not canonical outbe JSON v1")
        return manifest, raw
    except Exception as error:  # json/parser/schema diagnostics belong in evidence.
        differences.append(f"{label}: invalid release manifest: {error}")
        return None, raw


def _manifest_artifacts(manifest: dict[str, Any] | None) -> dict[str, dict[str, Any]]:
    if manifest is None:
        return {}
    return {artifact["name"]: artifact for artifact in manifest.get("artifacts", [])}


def verify_outputs(
    first: Path,
    second: Path,
    repo_root: Path,
    *,
    check_git_identity: bool = False,
) -> dict[str, Any]:
    """Return evidence with every mismatch. Never stop after the first one."""

    first = first.resolve()
    second = second.resolve()
    repo_root = repo_root.resolve()
    differences: list[str] = []
    _verify_environment(differences)

    schema = json.loads(
        (repo_root / "release/release-manifest-v1.schema.json").read_text(encoding="utf-8")
    )
    Draft202012Validator.check_schema(schema)
    first_manifest, first_manifest_raw = _read_manifest(first, schema, "builder-a", differences)
    second_manifest, second_manifest_raw = _read_manifest(second, schema, "builder-b", differences)

    if first_manifest_raw != second_manifest_raw:
        differences.append("release-manifest.json differs between builders")

    first_records = _manifest_artifacts(first_manifest)
    second_records = _manifest_artifacts(second_manifest)
    if tuple(first_records) != EXPECTED_ARTIFACTS:
        differences.append(
            "builder-a manifest does not declare the exact production ELF release matrix"
        )
    if tuple(second_records) != EXPECTED_ARTIFACTS:
        differences.append(
            "builder-b manifest does not declare the exact production ELF release matrix"
        )

    for label, output, manifest in (
        ("builder-a", first, first_manifest),
        ("builder-b", second, second_manifest),
    ):
        _materials.verify_manifest_materials(output, manifest, repo_root, label, differences)

    checksum_values = [
        _verify_checksum_file(first, "builder-a", differences),
        _verify_checksum_file(second, "builder-b", differences),
    ]
    if checksum_values[0] != checksum_values[1]:
        differences.append("SHA256SUMS differs between builders")

    version_values: list[bytes] = []
    for label, output, manifest in (
        ("builder-a", first, first_manifest),
        ("builder-b", second, second_manifest),
    ):
        version_values.append(
            _identity.verify_version_evidence(
                output, manifest, label, differences, version_verifier
            )
        )
    if version_values[0] != version_values[1]:
        differences.append("outbe-chain version evidence differs between builders")

    artifact_checks = _artifacts.ArtifactChecks(
        (first, second), (first_records, second_records), differences, globals()
    )
    artifact_evidence = [artifact_checks.compare(name) for name in EXPECTED_ARTIFACTS]
    identity = _identity.build_identity(first_manifest, repo_root, check_git_identity, differences)

    return {
        "artifacts": artifact_evidence,
        "build_identity": identity,
        "checks": [
            "manifest-schema-draft-2020-12",
            "manifest-canonical-bytes",
            "exact-production-elf-matrix",
            "per-artifact-manifest-digest-and-size",
            "source-input-manifest-digest-and-size",
            "resolved-package-inventory-digest-and-size",
            "exact-output-checksum-matrix",
            "elf-magic",
            "forbidden-absolute-build-paths",
            "forbidden-production-tee-markers",
            "pinned-verifier-environment",
            "outbe-chain-version-identity",
            "byte-for-byte-two-builder-comparison",
        ],
        "differences": differences,
        "manifest": {
            "builder_a_sha256": _sha256(first_manifest_raw) if first_manifest_raw else None,
            "builder_b_sha256": _sha256(second_manifest_raw) if second_manifest_raw else None,
            "byte_identical": first_manifest_raw == second_manifest_raw,
        },
        "output_checksums": {
            "builder_a_sha256": _sha256(checksum_values[0]) if checksum_values[0] else None,
            "builder_b_sha256": _sha256(checksum_values[1]) if checksum_values[1] else None,
            "byte_identical": checksum_values[0] == checksum_values[1],
        },
        "result": "passed" if not differences else "failed",
        "schema_version": "1.0.0",
        "verifier": {
            "requirements_sha256": _sha256(
                (repo_root / "release/reproducible-verifier-requirements.txt").read_bytes()
            ),
            "script_sha256": _sha256(Path(__file__).read_bytes()),
        },
    }


def _parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--first", required=True, type=Path)
    parser.add_argument("--second", required=True, type=Path)
    parser.add_argument("--repo-root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--output", required=True, type=Path)
    return parser.parse_args()


def main() -> None:
    args = _parse_args()
    evidence = verify_outputs(
        args.first,
        args.second,
        args.repo_root,
        check_git_identity=True,
    )
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_bytes(_canonical_json(evidence))
    print(f"reproducibility result: {evidence['result']}")
    for difference in evidence["differences"]:
        print(f"difference: {difference}")
    if evidence["result"] != "passed":
        raise SystemExit(1)


if __name__ == "__main__":
    main()
