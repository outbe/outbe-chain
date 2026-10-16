"""Bind TEE policy to the seeded genesis."""

from __future__ import annotations
import json
import shutil
import subprocess
from pathlib import Path
from typing import Any








def run_tee_stage(
    *,
    chain_binary: str,
    seeded_genesis: Path,
    output: Path,
    config: dict[str, Any],
) -> None:
    tee = config["tee"]
    cmd = [
        chain_binary,
        "tee",
        "genesis",
        "--input",
        str(seeded_genesis),
        "--output",
        str(output),
        "--mode",
        str(tee["mode"]),
    ]
    dcap_options = {
        "--mrenclave": tee.get("mrenclave"),
        "--mrsigner": tee.get("mrsigner"),
        "--isv-prod-id": tee.get("isv_prod_id"),
        "--minimum-isv-svn": tee.get("minimum_isv_svn"),
        "--minimum-tcb-evaluation-data-number": tee.get(
            "minimum_tcb_evaluation_data_number"
        ),
    }
    if tee["mode"] == "dcap-required":
        missing = sorted(name for name, value in dcap_options.items() if value is None)
        if missing:
            raise ValueError(
                "tee.mode dcap-required needs exact release values: "
                + ", ".join(missing)
            )
        for name, value in dcap_options.items():
            cmd.extend([name, str(value)])
    elif any(value is not None for value in dcap_options.values()):
        raise ValueError("tee.mode gramine-direct-dev forbids DCAP measurement values")
    subprocess.run(cmd, check=True)
