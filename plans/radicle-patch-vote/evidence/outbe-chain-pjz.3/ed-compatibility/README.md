# P2 actual Ed25519 compatibility after native BLS changes

Intermediate P2 evidence only. Source6c19aaaf514b7d5bd77c3de3559185459eaf425f. Reused the existing P1 peer driver unchanged; it starts real Outbe sidecars with temporary Ed users, performs actual CLI init/clone/helper ls-remote, verifies exact content/commit/identity/signed refs, and exercises bounded disconnect.

Both scenarios PASS: current→current and retained b76a178 legacy fork author→current receiver. The binaries were built on the current source, their version SHA is checked against HEAD, and hashes are captured in both PASS logs. The legacy version is independently required to match b76a178. Sidecar build is offline/locked. Commands, exact cwd/environment/platform/start/end/exit/log hashes are in commands.json; each peer directory includes complete control/CLI transcript and both sidecar logs. All processes/temp users are cleaned by the existing driver.

This proves ordinary Ed compatibility, including a real legacy peer, after native BLS changes. It does not claim BLS support on the legacy peer, a BLS downgrade, managed publication permission, P2 completion, a live chain or remote CI. BLS remains an explicit supported format; existing wire unit tests reject unsupported/downgrade paths.
