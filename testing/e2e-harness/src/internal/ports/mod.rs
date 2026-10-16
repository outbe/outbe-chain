//! Per-node port allocation for the localnet.
//!
//! Every node owns one **contiguous legacy block** of chain/OCOMP ports plus a
//! separate two-port Radicle block. Keeping the allocators separate prevents a
//! new sidecar from renumbering consensus ports embedded in existing fixtures.
//!
//! ```text
//! OCOMP reserves six consecutive TCP slots at the end of each node block:
//! Supervisor registration, Supervisor ZeroMQ, and four Worker observability ports.
//! ```
//!
//! The allocator assigns blocks from a cursor that only ever moves forward. So
//! the blocks are disjoint by construction, and no two services can collide. A
//! node index that the harness did not see before takes the next block on first
//! use. Examples are the joiner at `i = committee size` and the followers at
//! their high slots. So the harness does not need to know the committee size in
//! advance.
//!
//! By default, the allocator *scans* for a block: the cursor walks forward until it finds
//! [`BLOCK`] consecutive ports the OS reports free. The window slides as a unit,
//! so a block stays contiguous. `--no-resolve-ports` skips the scan and takes the
//! cursor's block verbatim.
//!
//! A block's ports follow from *allocation order*, not from the node index. So a
//! node whose first candidate port is busy shifts only itself, never the nodes
//! allocated after it.
//!
//! [`Ports::start_scenario`] forgets the node->block map but leaves the cursor
//! alone, so each scenario's nodes land above the previous scenario's. The
//! allocator never reuses a port within a process. This keeps a torn-down node's
//! lingering socket (or a peer still dialing it) from bleeding into the next
//! scenario.
//!
//! Bootstrap bakes the committee's consensus/p2p ports into `validators.json`/genesis.
//! So the allocator allocates blocks `0..n` at the scenario's start, and launch
//! reuses them unchanged. The cursor never rewinds, so a later block can't alias a
//! genesis-baked one.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use eyre::{bail, Result};

mod probe;

#[cfg(test)]
use probe::parse_ephemeral_port_range;
use probe::{ephemeral_port_range, is_free, radicle_window_free, window_free};

/// First port the allocator considers.
pub(crate) const NODE_BASE: u16 = 18545;

/// Which transport(s) a service needs free for the probe to consider a port open.
#[derive(Clone, Copy)]
enum Proto {
    Tcp,
    Udp,
    /// reth `--port` binds RLPx (TCP) and discv4 (UDP) on the same number.
    TcpUdp,
}

/// A service occupying one slot of a node's port block.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Service {
    Http,
    Tee,
    P2p,
    Discv5,
    Authrpc,
    Metrics,
    Consensus,
    Radicle,
    RadicleStatus,
    OcompSupervisor,
    OcompMessage,
    OcompWorker0,
    OcompWorker1,
    OcompWorker2,
    OcompWorker3,
    OcompSuccessorSupervisor,
    OcompSuccessorMessage,
    OcompSuccessorWorker0,
    OcompSuccessorWorker1,
    OcompSuccessorWorker2,
    OcompSuccessorWorker3,
    OcompSnapshotExporter,
}

impl Service {
    /// Every service known to collision and preflight checks.
    #[cfg(test)]
    const ALL: [Service; 22] = [
        Self::Http,
        Self::Tee,
        Self::P2p,
        Self::Discv5,
        Self::Authrpc,
        Self::Metrics,
        Self::Consensus,
        Self::OcompSupervisor,
        Self::OcompMessage,
        Self::OcompWorker0,
        Self::OcompWorker1,
        Self::OcompWorker2,
        Self::OcompWorker3,
        Self::OcompSuccessorSupervisor,
        Self::OcompSuccessorMessage,
        Self::OcompSuccessorWorker0,
        Self::OcompSuccessorWorker1,
        Self::OcompSuccessorWorker2,
        Self::OcompSuccessorWorker3,
        Self::OcompSnapshotExporter,
        Self::Radicle,
        Self::RadicleStatus,
    ];

    /// Complete chain/OCOMP runtime block. Derived OCOMP listeners must have an
    /// explicit slot here so allocation and preflight cannot miss them.
    const CORE: [Service; 20] = [
        Self::Http,
        Self::Tee,
        Self::P2p,
        Self::Discv5,
        Self::Authrpc,
        Self::Metrics,
        Self::Consensus,
        Self::OcompSupervisor,
        Self::OcompMessage,
        Self::OcompWorker0,
        Self::OcompWorker1,
        Self::OcompWorker2,
        Self::OcompWorker3,
        Self::OcompSuccessorSupervisor,
        Self::OcompSuccessorMessage,
        Self::OcompSuccessorWorker0,
        Self::OcompSuccessorWorker1,
        Self::OcompSuccessorWorker2,
        Self::OcompSuccessorWorker3,
        Self::OcompSnapshotExporter,
    ];

    const RADICLE: [Service; 2] = [Self::Radicle, Self::RadicleStatus];

    fn is_radicle(self) -> bool {
        matches!(self, Self::Radicle | Self::RadicleStatus)
    }

    /// This service's slot within a node's block.
    fn offset(self) -> u16 {
        match self {
            Self::Http => 0,
            Self::Tee => 1,
            Self::P2p => 2,
            Self::Discv5 => 3,
            Self::Authrpc => 4,
            Self::Metrics => 5,
            Self::Consensus => 6,
            Self::OcompSupervisor => 7,
            Self::OcompMessage => 8,
            Self::OcompWorker0 => 9,
            Self::OcompWorker1 => 10,
            Self::OcompWorker2 => 11,
            Self::OcompWorker3 => 12,
            Self::OcompSuccessorSupervisor => 13,
            Self::OcompSuccessorMessage => 14,
            Self::OcompSuccessorWorker0 => 15,
            Self::OcompSuccessorWorker1 => 16,
            Self::OcompSuccessorWorker2 => 17,
            Self::OcompSuccessorWorker3 => 18,
            Self::OcompSnapshotExporter => 19,
            Self::Radicle => 0,
            Self::RadicleStatus => 1,
        }
    }

    fn proto(self) -> Proto {
        match self {
            Self::P2p => Proto::TcpUdp,
            Self::Discv5 => Proto::Udp,
            _ => Proto::Tcp,
        }
    }
}

/// Ports per node block.
// Keep the established 21-port stride so removing an obsolete listener does
// not renumber the consensus ports baked into canonical fixtures.
const BLOCK: u16 = 21;
const RADICLE_BLOCK: u16 = Service::RADICLE.len() as u16;
const RADICLE_BASE: u16 = 50_000;
#[cfg(not(target_os = "macos"))]
const SCANNED_RADICLE_BASE: u16 = RADICLE_BASE;
// macOS's default ephemeral interval extends through u16::MAX. Start below it
// when scanning. The explicit static layout retains the same ports everywhere.
#[cfg(target_os = "macos")]
const SCANNED_RADICLE_BASE: u16 = 30_000;

/// Port allocator shared by every [`Config`](crate::internal::config::Config)
/// clone of a run.
///
/// The clones are not independent: `World::default()` hands one `Config` to each
/// of `Localnet`, `Rpc`, and `Validators`, and a block allocated through one must
/// look the same through the others. Hence the shared, interior-mutable resolver.
#[derive(Clone, Debug)]
pub(crate) struct Ports {
    inner: Arc<Mutex<Resolver>>,
}

#[derive(Debug)]
struct Resolver {
    /// Node index -> first port of its block.
    blocks: HashMap<usize, u16>,
    /// Node index -> first port of its separate Radicle block.
    radicle_blocks: HashMap<usize, u16>,
    /// Lowest port not yet handed out. Only ever moves forward.
    cursor: u64,
    /// Lowest Radicle port not yet handed out.
    radicle_cursor: u64,
    /// All issued spans, including earlier scenarios whose sockets may linger.
    issued: Vec<(u16, u16)>,
    /// Probe the OS for a free window, rather than taking the cursor verbatim.
    scan: bool,
}

impl Ports {
    /// An allocator with nothing handed out yet, its cursor at [`NODE_BASE`].
    ///
    /// `scan` probes the OS for each free window (the default). `--no-resolve-ports`
    /// disables it, which gives the static `NODE_BASE + i * BLOCK` layout.
    pub fn new(scan: bool) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Resolver {
                blocks: HashMap::new(),
                radicle_blocks: HashMap::new(),
                cursor: u64::from(NODE_BASE),
                radicle_cursor: u64::from(if scan {
                    SCANNED_RADICLE_BASE
                } else {
                    RADICLE_BASE
                }),
                issued: Vec::new(),
                scan,
            })),
        }
    }

    /// Reconstruct a previously resolved validator->service layout.
    ///
    /// The order is validator order. Each entry is the first port of that
    /// validator's complete [`BLOCK`]-wide service block.
    pub(crate) fn from_block_starts(starts: &[u16]) -> Result<Self> {
        for (index, &start) in starts.iter().enumerate() {
            fits(u64::from(start))?;
            let end = start + (BLOCK - 1);
            for (previous_index, &previous) in starts[..index].iter().enumerate() {
                let previous_end = previous + (BLOCK - 1);
                if start <= previous_end && previous <= end {
                    bail!(
                        "persisted validator-{index} port block {start}..={end} overlaps validator-{previous_index} block {previous}..={previous_end}"
                    );
                }
            }
        }
        let cursor = starts
            .iter()
            .copied()
            .max()
            .map_or(u64::from(NODE_BASE), |start| {
                u64::from(start) + u64::from(BLOCK)
            });
        Ok(Self {
            inner: Arc::new(Mutex::new(Resolver {
                blocks: starts.iter().copied().enumerate().collect(),
                radicle_blocks: HashMap::new(),
                cursor,
                radicle_cursor: u64::from(SCANNED_RADICLE_BASE),
                issued: starts
                    .iter()
                    .map(|&start| (start, start + (BLOCK - 1)))
                    .collect(),
                // Only CORE spans were persisted. Fresh sidecar/joiner ports
                // still need the normal OS collision and ephemeral checks.
                scan: true,
            })),
        })
    }

    /// Begin a scenario: forget the previous one's node->block map, then allocate
    /// the committee's blocks (`0..n`) so their consensus/p2p ports are fixed
    /// before bootstrap bakes them into genesis.
    ///
    /// The cursor is untouched, so this scenario's blocks sit above the last one's
    /// and no port is reused within the process.
    pub fn start_scenario(&self, n: usize) -> Result<()> {
        let mut r = lock(&self.inner);
        r.blocks.clear();
        r.radicle_blocks.clear();
        (0..n).try_for_each(|i| r.block_start(i).map(drop))
    }

    /// Return the complete ordered block layout for durable handoff to another
    /// harness process, allocating any missing committee blocks first.
    pub(crate) fn block_starts(&self, nodes: usize) -> Result<Vec<u16>> {
        let mut resolver = lock(&self.inner);
        (0..nodes)
            .map(|index| resolver.block_start(index))
            .collect()
    }

    /// The port node `i` uses for `svc`, allocating its block on first use.
    ///
    /// Panics only when the port space above the cursor is exhausted. That is an
    /// unrecoverable property of the machine, not of the caller.
    pub(crate) fn port(&self, svc: Service, i: usize) -> u16 {
        let mut resolver = lock(&self.inner);
        let start = if svc.is_radicle() {
            resolver.radicle_block_start(i)
        } else {
            resolver.block_start(i)
        }
        .unwrap_or_else(|e| panic!("e2e ports: {e}"));
        start + svc.offset()
    }

    /// Prove that every configured service port for `0..nodes` is still free.
    ///
    /// Persistent LocalNet restores bootstrap's resolved layout so genesis and
    /// the later owner process use identical ports. This preflight prevents an
    /// old or unrelated process from being mistaken for a newly owned node.
    #[cfg_attr(not(feature = "ocomp-integration"), allow(dead_code))]
    pub(crate) fn ensure_available(&self, nodes: usize) -> Result<()> {
        let mut resolver = lock(&self.inner);
        for index in 0..nodes {
            let start = resolver.block_start(index)?;
            for service in Service::CORE {
                let port = start + service.offset();
                if !is_free(port, service.proto()) {
                    bail!(
                        "LocalNet port preflight failed: validator-{index} {service:?} port {port} is already in use"
                    );
                }
            }
            let start = resolver.radicle_block_start(index)?;
            for service in Service::RADICLE {
                let port = start + service.offset();
                if !is_free(port, service.proto()) {
                    bail!(
                        "LocalNet port preflight failed: validator-{index} {service:?} port {port} is already in use"
                    );
                }
            }
        }
        Ok(())
    }
}

/// Lock the resolver, recovering from a poisoned mutex.
///
/// The resolver outlives any one scenario, so a panic in one (cucumber catches
/// them and continues) must not brick every later scenario's port lookups.
fn lock(inner: &Mutex<Resolver>) -> MutexGuard<'_, Resolver> {
    inner.lock().unwrap_or_else(|e| e.into_inner())
}

impl Resolver {
    /// First port of node `i`'s block: the one already allocated, or the next.
    fn block_start(&mut self, i: usize) -> Result<u16> {
        if let Some(&start) = self.blocks.get(&i) {
            return Ok(start);
        }
        let start = self.alloc()?;
        self.blocks.insert(i, start);
        Ok(start)
    }

    fn radicle_block_start(&mut self, i: usize) -> Result<u16> {
        if let Some(&start) = self.radicle_blocks.get(&i) {
            return Ok(start);
        }
        let start = self.alloc_radicle()?;
        self.radicle_blocks.insert(i, start);
        Ok(start)
    }

    /// Take the next block at or above the cursor, and advance the cursor past it.
    fn alloc(&mut self) -> Result<u16> {
        let mut candidate = self.cursor;
        loop {
            let start = fits(candidate)?;
            if !self.overlaps_issued(start, BLOCK) && (!self.scan || window_free(start)) {
                self.cursor = u64::from(start) + u64::from(BLOCK);
                self.issued.push((start, start + (BLOCK - 1)));
                return Ok(start);
            }
            candidate = u64::from(start) + 1;
        }
    }

    fn alloc_radicle(&mut self) -> Result<u16> {
        let ephemeral = if self.scan {
            Some(ephemeral_port_range()?)
        } else {
            None
        };
        self.alloc_radicle_avoiding(ephemeral)
    }

    fn alloc_radicle_avoiding(&mut self, ephemeral: Option<(u16, u16)>) -> Result<u16> {
        let mut candidate = self.radicle_cursor;
        loop {
            let start = fits_width(candidate, RADICLE_BLOCK)?;
            if let Some((low, high)) = ephemeral {
                if ranges_overlap((start, start + (RADICLE_BLOCK - 1)), (low, high)) {
                    candidate = u64::from(high) + 1;
                    continue;
                }
            }
            if !self.overlaps_issued(start, RADICLE_BLOCK)
                && (!self.scan || radicle_window_free(start))
            {
                self.radicle_cursor = u64::from(start) + u64::from(RADICLE_BLOCK);
                self.issued.push((start, start + (RADICLE_BLOCK - 1)));
                return Ok(start);
            }
            candidate = u64::from(start) + 1;
        }
    }

    fn overlaps_issued(&self, start: u16, width: u16) -> bool {
        self.issued
            .iter()
            .any(|&span| ranges_overlap((start, start + (width - 1)), span))
    }
}

fn ranges_overlap(left: (u16, u16), right: (u16, u16)) -> bool {
    left.0 <= right.1 && right.0 <= left.1
}

/// `start` as a `u16`, once we know a whole block fits at or above it.
fn fits(start: u64) -> Result<u16> {
    fits_width(start, BLOCK)
}

fn fits_width(start: u64, width: u16) -> Result<u16> {
    if start + u64::from(width) - 1 > u64::from(u16::MAX) {
        bail!("no free {width}-port block at or above {start}");
    }
    Ok(start as u16)
}

#[cfg(test)]
mod tests;
