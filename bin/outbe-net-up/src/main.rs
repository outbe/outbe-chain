mod cli;
mod network;

use std::process::ExitCode;

use clap::Parser;

#[tokio::main]
async fn main() -> ExitCode {
    match cli::Cli::parse().run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            if let Some(exit_err) = err.downcast_ref::<network::vm::ExitCodeError>() {
                if !exit_err.message.is_empty() {
                    eprintln!("{}", exit_err.message);
                }
                ExitCode::from(exit_err.code)
            } else {
                eprintln!("Error: {err:?}");
                ExitCode::FAILURE
            }
        }
    }
}
