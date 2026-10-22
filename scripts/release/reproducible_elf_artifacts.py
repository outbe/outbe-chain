from __future__ import annotations

from pathlib import Path
from typing import Any


class ArtifactChecks:
    def __init__(
        self,
        outputs: tuple[Path, Path],
        records: tuple[dict, dict],
        differences: list[str],
        checks: dict[str, Any],
    ):
        self.outputs = outputs
        self.records = records
        self.differences = differences
        self.checks = checks

    def verify_content(self, raw: bytes, label: str, name: str) -> None:
        if not raw.startswith(b"\x7fELF"):
            self.differences.append(f"{label}: {name} is not an ELF file")
        for forbidden in self.checks["FORBIDDEN_PATHS"]:
            if forbidden in raw:
                self.differences.append(
                    f"{label}: forbidden absolute build path in {name}: {forbidden.decode()}"
                )
        if name == "outbe-tee-enclave":
            for marker in self.checks["PRODUCTION_TEE_FORBIDDEN_MARKERS"]:
                if marker in raw:
                    self.differences.append(
                        f"{label}: forbidden production TEE marker: {marker.decode('ascii')}"
                    )

    def verify_record(self, record: dict | None, raw: bytes, label: str, name: str) -> None:
        if record is None:
            return
        recorded_hash = record.get("digest", {}).get("value")
        if recorded_hash != self.checks["_sha256"](raw):
            self.differences.append(f"{label}: manifest digest mismatch for {name}")
        if record.get("size") != len(raw):
            self.differences.append(f"{label}: manifest size mismatch for {name}")

    def compare(self, name: str) -> dict[str, Any]:
        paths = tuple(output / "bin" / name for output in self.outputs)
        raw_values: list[bytes] = []
        hashes: list[str | None] = []
        sizes: list[int | None] = []
        for label, path, records in zip(
            ("builder-a", "builder-b"), paths, self.records, strict=True
        ):
            if not path.is_file() or path.is_symlink():
                self.differences.append(f"{label}: missing regular ELF: {name}")
                raw_values.append(b"")
                hashes.append(None)
                sizes.append(None)
                continue
            raw = path.read_bytes()
            raw_values.append(raw)
            hashes.append(self.checks["_sha256"](raw))
            sizes.append(len(raw))
            self.verify_content(raw, label, name)
            self.verify_record(records.get(name), raw, label, name)
        if raw_values[0] != raw_values[1]:
            self.differences.append(f"ELF bytes differ between builders: {name}")
        return {
            "builder_a_sha256": hashes[0],
            "builder_b_sha256": hashes[1],
            "byte_identical": raw_values[0] == raw_values[1],
            "name": name,
            "size": sizes[0] if sizes[0] == sizes[1] else None,
        }
