#!/usr/bin/env python3
"""Verify saved `outbe-chain --version` output without executing an external candidate."""

from __future__ import annotations

import argparse
import json
from datetime import UTC, datetime
from pathlib import Path


def expected_timestamp(source_date_epoch: int) -> str:
    timestamp = datetime.fromtimestamp(source_date_epoch, tz=UTC)
    return timestamp.strftime("%Y-%m-%dT%H:%M:%S.000000000Z")


def verify_version_text(
    text: str,
    *,
    source_commit: str,
    source_date_epoch: int,
    target: str,
    profile: str,
    features: list[str] | None = None,
) -> list[str]:
    differences: list[str] = []
    lines = text.splitlines()
    expected_commit = f"Commit SHA: {source_commit}"
    expected_time = f"Build Timestamp: {expected_timestamp(source_date_epoch)}"
    expected_profile = f"Build Profile: {profile} ({target})"

    if text.count(expected_commit) != 2:
        differences.append(f"expected two exact commit lines: {expected_commit}")
    if text.count(expected_time) != 2:
        differences.append(f"expected two exact deterministic timestamp lines: {expected_time}")
    if expected_profile not in text:
        differences.append(f"missing exact Outbe target/profile line: {expected_profile}")
    # Vergen derives this value from CARGO_FEATURE_* environment keys: hyphens
    # become underscores, and multiple features are joined without spaces.
    expected_features = "Build Features: " + (
        ",".join(sorted(feature.replace("-", "_") for feature in features))
        if features else "no features enabled"
    )
    if lines.count(expected_features) != 1:
        differences.append(f"expected one exact Outbe feature line: {expected_features}")
    return differences


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version-file", required=True, type=Path)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--source-date-epoch", required=True, type=int)
    parser.add_argument("--target", required=True)
    parser.add_argument("--profile", required=True)
    parser.add_argument("--build-spec", type=Path)
    args = parser.parse_args()

    features = None
    if args.build_spec:
        spec = json.loads(args.build_spec.read_text(encoding="utf-8"))
        features = next(a["features"] for a in spec["artifacts"] if a["name"] == "outbe-chain")

    differences = verify_version_text(
        args.version_file.read_text(encoding="utf-8"),
        source_commit=args.source_commit,
        source_date_epoch=args.source_date_epoch,
        target=args.target,
        profile=args.profile,
        features=features,
    )
    for difference in differences:
        print(f"version identity mismatch: {difference}")
    if differences:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
