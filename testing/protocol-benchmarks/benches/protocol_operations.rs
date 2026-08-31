use std::path::PathBuf;

use outbe_gemfactory::GemTypes;
use outbe_protocol_benchmarks::{
    check_gas_baseline, render_text, run_scenario,
    scenarios::confidential::ConfidentialScenario,
    scenarios::credis::CredisScenario,
    scenarios::credis_boundary::CredisVaultBoundaryScenario,
    scenarios::gem::GemScenario,
    scenarios::intex::IntexScenario,
    scenarios::metadosis::MetadosisScenario,
    scenarios::nod::NodScenario,
    scenarios::stablecoin::StablecoinScenario,
    scenarios::system_tx::SystemTxScenario,
    scenarios::tribute::{render_gas_policy, TributeScenario},
    update_gas_baseline, write_json_atomic, BenchmarkRunReport, BenchmarkScenario, GasBaseline,
    Profile, RunConfig, ScenarioReport,
};

const DEFAULT_SAMPLES: usize = 30;

enum Command {
    Run,
    BaselineCheck,
    BaselineUpdate,
}

struct Arguments {
    command: Command,
    samples: usize,
    filter: String,
    json: Option<PathBuf>,
    baseline: PathBuf,
}

fn main() {
    if let Err(error) = execute() {
        eprintln!("protocol benchmark failed: {error}");
        std::process::exit(2);
    }
}

fn execute() -> Result<(), String> {
    let arguments = parse_arguments()?;
    let config = RunConfig {
        samples: arguments.samples,
        warmups: 3,
    };
    let mut reports = Vec::new();
    for scenario in [TributeScenario::non_zk(), TributeScenario::zk()] {
        run_if_selected(&scenario, &arguments.filter, config, &mut reports)?;
    }
    for scenario in [
        NodScenario::direct(Profile::Single),
        NodScenario::direct(Profile::Typical),
        NodScenario::certified(Profile::Single),
        NodScenario::certified(Profile::Typical),
    ] {
        run_if_selected(&scenario, &arguments.filter, config, &mut reports)?;
    }
    for scenario in [
        StablecoinScenario::reserve(),
        StablecoinScenario::approved(),
    ] {
        run_if_selected(&scenario, &arguments.filter, config, &mut reports)?;
    }
    run_if_selected(
        &MetadosisScenario::worldwide_day(),
        &arguments.filter,
        config,
        &mut reports,
    )?;
    run_if_selected(
        &CredisScenario::request(),
        &arguments.filter,
        config,
        &mut reports,
    )?;
    run_if_selected(
        &CredisVaultBoundaryScenario,
        &arguments.filter,
        config,
        &mut reports,
    )?;
    for scenario in [
        ConfidentialScenario::promis(),
        ConfidentialScenario::gratis(),
        ConfidentialScenario::gratis_with_fidelity(),
        ConfidentialScenario::gratisfactory(),
    ] {
        run_if_selected(&scenario, &arguments.filter, config, &mut reports)?;
    }
    for scenario in [
        IntexScenario::issue(Profile::Single),
        IntexScenario::issue(Profile::Typical),
        IntexScenario::send(Profile::Single),
        IntexScenario::send(Profile::Typical),
    ] {
        run_if_selected(&scenario, &arguments.filter, config, &mut reports)?;
    }
    for scenario in [
        GemScenario::direct(GemTypes::Genesis),
        GemScenario::direct(GemTypes::Validator),
        GemScenario::direct(GemTypes::Sra),
        GemScenario::direct(GemTypes::Wallet),
        GemScenario::direct(GemTypes::Cca),
        GemScenario::position(),
        GemScenario::merchant(),
    ] {
        run_if_selected(&scenario, &arguments.filter, config, &mut reports)?;
    }
    for scenario in SystemTxScenario::all() {
        run_if_selected(&scenario, &arguments.filter, config, &mut reports)?;
    }
    if reports.is_empty() {
        return Err(format!(
            "filter {:?} selected no benchmark scenarios",
            arguments.filter
        ));
    }

    let run_report = BenchmarkRunReport::new(reports);
    print!("{}", render_text(&run_report));
    if let Some(policy) = render_gas_policy(&run_report.scenarios) {
        print!("{policy}");
    }
    if let Some(path) = arguments.json {
        write_json_atomic(&path, &run_report).map_err(|error| error.to_string())?;
    }

    let baseline =
        GasBaseline::from_reports(2, &run_report.scenarios).map_err(|error| error.to_string())?;
    match arguments.command {
        Command::Run => {}
        Command::BaselineCheck => {
            check_gas_baseline(&arguments.baseline, &baseline).map_err(|error| error.to_string())?
        }
        Command::BaselineUpdate => update_gas_baseline(&arguments.baseline, &baseline)
            .map_err(|error| error.to_string())?,
    }
    Ok(())
}

fn run_if_selected<S: BenchmarkScenario>(
    scenario: &S,
    filter: &str,
    config: RunConfig,
    reports: &mut Vec<ScenarioReport>,
) -> Result<(), String> {
    let metadata = scenario.metadata();
    if filter == "all" || metadata.id.contains(filter) {
        reports.push(run_scenario(scenario, config).map_err(|error| error.to_string())?);
    }
    Ok(())
}

fn parse_arguments() -> Result<Arguments, String> {
    let mut command = Command::Run;
    let mut samples = std::env::var("PROTOCOL_BENCH_SAMPLES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_SAMPLES);
    let mut filter = "all".to_owned();
    let mut json = None;
    let mut baseline = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("baselines/gas-v1.json");
    let mut arguments = std::env::args()
        .skip(1)
        .filter(|argument| argument != "--bench");
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "run" => command = Command::Run,
            "baseline-check" => command = Command::BaselineCheck,
            "baseline-update" => command = Command::BaselineUpdate,
            "--samples" => {
                samples = arguments
                    .next()
                    .ok_or("--samples requires a value")?
                    .parse()
                    .map_err(|_| "--samples must be an integer")?;
            }
            "--filter" => filter = arguments.next().ok_or("--filter requires a value")?,
            "--json" => {
                json = Some(PathBuf::from(
                    arguments.next().ok_or("--json requires a path")?,
                ));
            }
            "--baseline" => {
                baseline = PathBuf::from(arguments.next().ok_or("--baseline requires a path")?);
            }
            unknown => return Err(format!("unknown argument {unknown:?}")),
        }
    }
    if samples < 3 {
        return Err(format!("sample count must be at least 3, got {samples}"));
    }
    Ok(Arguments {
        command,
        samples,
        filter,
        json,
        baseline,
    })
}
