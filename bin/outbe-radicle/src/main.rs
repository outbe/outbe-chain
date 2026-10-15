mod errors;

use std::sync::mpsc;

use crate::errors::RadicleBinaryError;

fn main() {
    let _ = log::set_boxed_logger(Box::new(radicle_log::Logger::new()));
    log::set_max_level(log::LevelFilter::Info);
    if let Err(error) = run() {
        eprintln!("outbe-radicle: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), RadicleBinaryError> {
    let options = outbe_radicle_runtime::Options::from_env()?;
    let (sender, receiver) = mpsc::sync_channel(1);
    radicle_signals::install(sender)?;
    Ok(outbe_radicle_runtime::PreparedSidecar::prepare(options, receiver)?.run()?)
}
