use outbe_tee_enclave::errors::{EnclaveResult, GramineError, SgxSealingError};

fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: probe seal|unseal|quote FILE");
        std::process::exit(2);
    }
    let result = (|| -> EnclaveResult<()> {
        let report = std::fs::read("/dev/attestation/report").map_err(SgxSealingError::other)?;
        if report.len() != 432 {
            return Err(SgxSealingError::InvalidHardwareReport.into());
        }
        println!("MRENCLAVE={}", hex::encode(&report[64..96]));
        println!("MRSIGNER={}", hex::encode(&report[128..160]));
        if args[1] == "quote" {
            let data = [0x73; 64]; // Public fixture data only.
            let quote = outbe_tee_enclave::gramine::dcap_quote(&data)?;
            let measurements = outbe_tee_enclave::gramine::parse_quote_measurements(&quote)
                .map_err(GramineError::quote_measurements)?;
            if measurements.report_data != data {
                return Err(SgxSealingError::QuoteReportDataDiffers.into());
            }
            std::fs::write(&args[2], &quote).map_err(SgxSealingError::other)?;
            println!("DCAP_QUOTE_BYTES={}", quote.len());
            Ok(())
        } else {
            outbe_tee_enclave::sgx_sealing::hardware_probe(&args[1], std::path::Path::new(&args[2]))
        }
    })();
    match result {
        Ok(()) => println!("SGX_COMBINED_SEAL_PROBE_OK"),
        Err(error) => {
            eprintln!("SGX_COMBINED_SEAL_PROBE_REJECTED: {error}");
            std::process::exit(6);
        }
    }
}
