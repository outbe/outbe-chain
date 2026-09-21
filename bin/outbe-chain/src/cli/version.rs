/// Outbe build metadata block printed before delegating `--version` to
/// Reth's CLI. Layout mirrors reth-node-core / kona-node so operators
/// see a familiar five-line block.
const OUTBE_LONG_VERSION: &str = concat!(
    env!("OUTBE_LONG_VERSION_0"),
    "\n",
    env!("OUTBE_LONG_VERSION_1"),
    "\n",
    env!("OUTBE_LONG_VERSION_2"),
    "\n",
    env!("OUTBE_LONG_VERSION_3"),
    "\n",
    env!("OUTBE_LONG_VERSION_4"),
);

/// Print Outbe build metadata baked in by `build.rs`. Followed downstream by
/// Reth's own `--version` output.
pub(crate) fn print_outbe_version() {
    println!("Outbe {}", env!("OUTBE_SHORT_VERSION"));
    println!("{OUTBE_LONG_VERSION}");
    println!();
}
