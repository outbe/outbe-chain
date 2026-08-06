//! `outbe-e2e` — the e2e runner binary.
//!
//! The CLI defines the *environment* (`--validators`, `--tee`, `--no-sudo`) and
//! the run policy (`--all`); Gherkin tags define each scenario's *requirements*.
//! All the wiring lives in [`outbe_e2e_harness::run`].

#[tokio::main]
async fn main() {
    let lane = match std::env::args().nth(1).as_deref() {
        Some("localnet") => Some(outbe_e2e_harness::localnet_driver::LocalnetLane::DevMock),
        Some("localnet-sgx") => Some(outbe_e2e_harness::localnet_driver::LocalnetLane::RealSgx),
        _ => None,
    };
    if let Some(lane) = lane {
        let program = match lane {
            outbe_e2e_harness::localnet_driver::LocalnetLane::DevMock => "outbe-e2e localnet",
            outbe_e2e_harness::localnet_driver::LocalnetLane::RealSgx => "outbe-e2e localnet-sgx",
        };
        let args =
            std::iter::once(std::ffi::OsString::from(program)).chain(std::env::args_os().skip(2));
        if let Err(error) = outbe_e2e_harness::localnet_driver::run_from(lane, args).await {
            eprintln!("{error:#}");
            std::process::exit(1);
        }
        return;
    }
    outbe_e2e_harness::run().await;
}
