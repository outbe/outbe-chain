Source with identical tested code bytes: `9500ce8c35a4d6fedd2ab9ea0aa970ecb887808c`. Tests were run immediately before
this commit, with only these three source files staged; no later source edit occurred.
109 protocol tests and whole vendored workspace/all-target Clippy (-D warnings) passed.
This follow-up makes generic Frame<RefsAt>/Frame<RefsAnnouncement> select/decode V2,
while RefsAnnouncement V1 retains its exact legacy OID-prefixed encoding.
The parent frontier648 tests and real peer binary evidence remain pinned to f1c224eda.
Full P2 acceptance is still pending; no newer two-node binary run is claimed here.
