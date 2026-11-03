#[path = "../release_dcap/mod.rs"]
mod release_dcap;

fn main() -> eyre::Result<()> {
    release_dcap::main()
}
