#!/usr/bin/env python3
"""Local four-validator Vote duration E2E, including an enclave upgrade.

Uses disposable keys, data directories, loopback ports and software enclave fixtures.
Never points at an existing network. The 1000/30000-block cases check creation and
restart persistence. Short windows exercise expiry, approval and Update activation.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import time

from enclave_upgrade_e2e import Network, UPDATE

VOTE = "0x000000000000000000000000000000000000ee0c"


class VoteDurationNetwork(Network):
    def read_contract(self, address, signature, values=(), block="finalized", node=0):
        data = subprocess.check_output(
            ["cast", "calldata", signature, *map(str, values)], text=True
        ).strip()
        return self.rpc("eth_call", [{"to": address, "data": data}, block], node)

    def proposal(self, proposal_id, block="finalized", node=0):
        raw = bytes.fromhex(self.read_contract(
            VOTE, "getProposal(uint256)", [proposal_id], block, node
        )[2:])
        # IVote.ProposalInfo is a dynamic ABI tuple. Its first ten head words
        # contain the id, addresses, payload offset, heights, status and tally.
        start = int.from_bytes(raw[:32], "big")
        words = [int.from_bytes(raw[start + i * 32:start + (i + 1) * 32], "big")
                 for i in range(10)]
        assert words[0] == proposal_id, words
        return {"id": words[0], "created": words[4], "deadline": words[5],
                "status": words[6], "yes": words[7], "no": words[8]}

    def propose(self, proposal_id, author, window, version="0.2", activation=100_000):
        payload = {"version": version, "activationHeight": activation, "info": "vote duration E2E"}
        if window is not None:
            payload["votingWindowBlocks"] = window
        path = self.directory / f"proposal-{proposal_id}.json"
        path.write_text(json.dumps(payload))
        receipt = self.wait_transaction(self.cli(
            ["vote", "propose", "--target-module", UPDATE, "--payload-file", path],
            i=author, label=f"propose-{proposal_id}",
        ))
        created = int(receipt["blockNumber"], 16)
        self.wait_height(created, f"proposal-{proposal_id}-finalized")
        expected = window if window is not None else 86_400
        records = [self.proposal(proposal_id, node=i) for i in range(4)]
        for record in records:
            assert record["created"] == created, record
            assert record["deadline"] == created + expected, record
            assert record["status"] == 0, record
        assert all(record == records[0] for record in records), records
        self.report.setdefault("proposals", []).append({"window": expected, **records[0]})
        return records[0]

    def status_on_all(self, proposal_id, expected, block="finalized"):
        records = [self.proposal(proposal_id, block, i) for i in range(4)]
        assert all(record["status"] == expected for record in records), records
        return records

    def reject_creation(self, window):
        payload = json.dumps({"version": "0.2", "activationHeight": 100_000,
                              "votingWindowBlocks": window})
        label = f"reject-window-{window}"
        try:
            self.cli(["vote", "propose", "--target-module", UPDATE, "--payload", payload],
                     i=3, label=label)
        except RuntimeError:
            log = (self.directory / f"{label}.log").read_text()
            assert "transaction sent:" not in log, log
            assert "votingWindowBlocks" in log or "voting window" in log, log
        else:
            raise AssertionError(f"invalid voting window accepted: {window}")

    def execute(self):
        self.prepare()
        for i in range(4):
            self.start_sidecar(i)
            self.start_enclave(i)
        time.sleep(2)
        for i in range(4):
            self.start_node(i)
        height = self.wait_height(5, "old-network-bootstrap")
        legacy = self.propose(1, 0, None)
        for i in range(4):
            self.stop(f"node-{i}")
            self.start_node(i, new=True)
            height = self.wait_height(height + 3, f"node-{i}-updated")
        assert self.proposal(1) == legacy
        thousand = self.propose(2, 1, 1_000)
        thirty_thousand = self.propose(3, 2, 30_000)
        height = int(self.rpc("eth_blockNumber"), 16)
        self.stop("node-0")
        self.start_node(0, new=True)
        self.wait_height(height + 3, "restart-preserves-proposal-deadlines")
        for proposal_id, expected in [(1, legacy), (2, thousand), (3, thirty_thousand)]:
            assert all(record == expected for record in self.status_on_all(proposal_id, 0))
        for window in [0, -1, 86_401]:
            self.reject_creation(window)
        expired = self.propose(4, 3, 16)
        self.wait_transaction(self.cli(["vote", "cast", "--proposal-id", "4", "--yes"],
                                      i=3, label="single-vote-no-quorum"))
        self.wait_height(expired["deadline"] + 1, "no-quorum-expired")
        records = self.status_on_all(4, 3)  # IVote.Expired
        assert all(record["yes"] == 1 for record in records), records
        height = int(self.rpc("eth_blockNumber"), 16)
        # Devnet requires a 100-block buffer after approval.
        activation = height + 150
        approved = self.propose(5, 3, 20, version="0.1", activation=activation)
        for i in range(3):
            self.wait_transaction(self.cli(["vote", "cast", "--proposal-id", "5", "--yes"],
                                          i=i, label=f"approve-{i}"))
        self.wait_height(approved["deadline"] + 1, "short-vote-approved")
        # Check the exact deadline using historical state on every validator.
        self.status_on_all(5, 0, hex(approved["deadline"]))
        records = self.status_on_all(5, 1)
        assert all(record["yes"] == 3 for record in records), records
        self.wait_height(activation + 2, "software-update-activated", timeout=600)
        for i in range(4):
            assert int(self.read_contract(UPDATE, "getActiveVersion()", block=hex(activation-1), node=i), 16) == 0
            assert int(self.read_contract(UPDATE, "getActiveVersion()", node=i), 16) == 1
            assert int(self.read_contract(UPDATE, "getActiveVersionHeight()", node=i), 16) == activation
        for proposal_id, expected in [(1, legacy), (2, thousand), (3, thirty_thousand)]:
            assert all(record == expected for record in self.status_on_all(proposal_id, 0))
        assert hashlib.sha256((self.network / "genesis.json").read_bytes()).hexdigest() == self.genesis_digest
        self.report["checks"] = ["legacy-proposal-before-binary-upgrade", "duration-1000-and-30000",
                                 "restart-persistence", "invalid-duration-rejected", "expiry-without-quorum",
                                 "inclusive-deadline", "approval-with-quorum", "separate-update-activation"]
        self.report["result"] = "passed"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("old-node", "new-node", "cli", "keygen", "enclave", "radicle", "output"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--port-offset", type=int, default=3000)
    args = parser.parse_args()
    for name in ("old_node", "new_node", "cli", "keygen", "enclave", "radicle"):
        path = getattr(args, name).resolve()
        if not path.is_file():
            parser.error(f"missing executable: {path}")
        setattr(args, name, path)
    args.output = args.output.resolve()
    if args.output.exists():
        parser.error("output must be a fresh directory")
    args.transition_node = args.new_node
    args.epoch_length_blocks = 300
    args.cross_epoch = False
    args.inject_proof_faults = False
    args.governance_window_blocks = 86_400
    args.proposal_voting_window_blocks = 20
    for network_type, scenario in [(VoteDurationNetwork, "vote-duration"), (Network, "enclave")]:
        network = network_type(args, scenario)
        network.report["network_voting_window_blocks"] = args.governance_window_blocks
        try:
            network.execute()
        except BaseException as error:
            network.report.update(result="failed", error=str(error))
            raise
        finally:
            try:
                network.close()
            except BaseException as error:
                network.report.update(result="failed", cleanup_error=str(error))
                raise
            finally:
                (network.directory / "result.json").write_text(json.dumps(network.report, indent=2) + "\n")
        print(f"{scenario}: PASSED (including updated-node shutdown checks)", flush=True)


if __name__ == "__main__":
    main()
