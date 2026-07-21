#!/usr/bin/env python3
"""Compare two clean Outbe ELF build outputs and save canonical evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
from typing import Any

from jsonschema import Draft202012Validator


EXPECTED_ARTIFACTS = (
    "outbe-chain",
    "outbe-cli",
    "outbe-keygen",
    "outbe-feeder",
    "outbe-tee-enclave",
)
FORBIDDEN_PATHS = (b"/workspace", b"/usr/local/cargo", b"/usr/local/rustup")


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


def verify_outputs(first: Path, second: Path, repo_root: Path) -> dict[str, Any]:
    """Return evidence with every mismatch; never stop after the first one."""

    first = first.resolve()
    second = second.resolve()
    repo_root = repo_root.resolve()
    differences: list[str] = []

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
        differences.append("builder-a manifest does not declare the exact five-ELF release matrix")
    if tuple(second_records) != EXPECTED_ARTIFACTS:
        differences.append("builder-b manifest does not declare the exact five-ELF release matrix")

    artifact_evidence = []
    for name in EXPECTED_ARTIFACTS:
        paths = (first / "bin" / name, second / "bin" / name)
        raw_values: list[bytes] = []
        hashes: list[str | None] = []
        sizes: list[int | None] = []
        for label, path in zip(("builder-a", "builder-b"), paths, strict=True):
            if not path.is_file() or path.is_symlink():
                differences.append(f"{label}: missing regular ELF: {name}")
                raw_values.append(b"")
                hashes.append(None)
                sizes.append(None)
                continue
            raw = path.read_bytes()
            raw_values.append(raw)
            hashes.append(_sha256(raw))
            sizes.append(len(raw))
            if not raw.startswith(b"\x7fELF"):
                differences.append(f"{label}: {name} is not an ELF file")
            for forbidden in FORBIDDEN_PATHS:
                if forbidden in raw:
                    differences.append(
                        f"{label}: forbidden absolute build path in {name}: {forbidden.decode()}"
                    )

            record = (first_records if label == "builder-a" else second_records).get(name)
            if record is not None:
                recorded_hash = record.get("digest", {}).get("value")
                if recorded_hash != hashes[-1]:
                    differences.append(f"{label}: manifest digest mismatch for {name}")
                if record.get("size") != sizes[-1]:
                    differences.append(f"{label}: manifest size mismatch for {name}")

        if raw_values[0] != raw_values[1]:
            differences.append(f"ELF bytes differ between builders: {name}")
        artifact_evidence.append(
            {
                "builder_a_sha256": hashes[0],
                "builder_b_sha256": hashes[1],
                "byte_identical": raw_values[0] == raw_values[1],
                "name": name,
                "size": sizes[0] if sizes[0] == sizes[1] else None,
            }
        )

    identity: dict[str, Any] = {}
    if first_manifest is not None:
        identity = {
            "release_tag": first_manifest["release"]["tag"],
            "source_commit": first_manifest["release"]["source"]["commit"],
            "source_date_epoch": first_manifest["build"]["source_date_epoch"],
            "target": first_manifest["build"]["target"],
            "profile": first_manifest["build"]["profile"],
            "builder_image": first_manifest["build"]["builder"]["image"],
        }

    return {
        "artifacts": artifact_evidence,
        "build_identity": identity,
        "checks": [
            "manifest-schema-draft-2020-12",
            "manifest-canonical-bytes",
            "exact-five-elf-matrix",
            "per-artifact-manifest-digest-and-size",
            "elf-magic",
            "forbidden-absolute-build-paths",
            "byte-for-byte-two-builder-comparison",
        ],
        "differences": differences,
        "manifest": {
            "builder_a_sha256": _sha256(first_manifest_raw) if first_manifest_raw else None,
            "builder_b_sha256": _sha256(second_manifest_raw) if second_manifest_raw else None,
            "byte_identical": first_manifest_raw == second_manifest_raw,
        },
        "result": "passed" if not differences else "failed",
        "schema_version": "1.0.0",
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
    evidence = verify_outputs(args.first, args.second, args.repo_root)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_bytes(_canonical_json(evidence))
    print(f"reproducibility result: {evidence['result']}")
    for difference in evidence["differences"]:
        print(f"difference: {difference}")
    if evidence["result"] != "passed":
        raise SystemExit(1)


if __name__ == "__main__":
    main()
