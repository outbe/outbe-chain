//! Calls the actual checked Rust coefficient and Fidelity arithmetic.
extern crate self as outbe_primitives;
pub mod error {
    #[derive(Debug)]
    pub enum PrecompileError {
        Revert(String),
    }
    pub type Result<T> = std::result::Result<T, PrecompileError>;
}
pub mod units {
    pub const SCALE_1E6_U128: u128 = 1_000_000;
    pub const SCALE_1E6_U256: alloy_primitives::U256 =
        alloy_primitives::U256::from_limbs([1_000_000, 0, 0, 0]);
    pub const SCALE_1E18: alloy_primitives::U256 =
        alloy_primitives::U256::from_limbs([1_000_000_000_000_000_000, 0, 0, 0]);
}
#[allow(dead_code)]
mod algorithm {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../crates/core/lysis/src/algorithm.rs"
    ));
}
#[allow(dead_code)]
#[path = "../../../../../crates/core/fidelity-math/src/lib.rs"]
mod fidelity;
use alloy_primitives::U256;
use num_bigint::BigUint;
use num_traits::Zero;
use outbe_private_lifecycle_poc::crypto::*;
use serde_json::{json, Value};
use std::path::Path;
fn u(s: &str) -> Result<U256> {
    Ok(s.parse()?)
}
fn run() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let j: Value = serde_json::from_slice(&std::fs::read(&args[0])?)?;
    let out = match j["op"].as_str().ok_or("op")? {
        "decay" => {
            json!({"values":j["ages"].as_array().ok_or("ages")?.iter().map(|x|fidelity::t_dec(x.as_u64().unwrap()).to_string()).collect::<Vec<_>>() })
        }
        "fidelity" => {
            let now = j["now"].as_u64().ok_or("now")?;
            let mut a = fidelity::RcfiAccumulator::default();
            let mut valid = true;
            for x in j["active"].as_array().ok_or("active")? {
                valid &= a
                    .add_active(
                        u(x["value"].as_str().ok_or("value")?)?,
                        x["at"].as_u64().ok_or("at")?,
                        now,
                    )
                    .is_some();
            }
            for x in j["sold"].as_array().ok_or("sold")? {
                valid &= a
                    .add_sold(
                        u(x["value"].as_str().ok_or("value")?)?,
                        x["at"].as_u64().ok_or("at")?,
                        x["sold_at"].as_u64().ok_or("sold_at")?,
                        now,
                    )
                    .is_some();
            }
            let result = a.finish(j["qualified"].as_u64().ok_or("qualified")?, now);
            if let Some((rcfi, efficiency, age)) = result {
                json!({"valid":valid,"rcfi":rcfi.to_string(),"efficiency":efficiency.to_string(),"age":age.to_string(),"league":fidelity::league_from_rcfi(rcfi,u(j["maximum"].as_str().ok_or("maximum")?)?)})
            } else {
                json!({"valid":false})
            }
        }
        "lysis" => {
            let sums: Vec<String> = serde_json::from_value(j["sums"].clone())?;
            let counts: Vec<u64> = serde_json::from_value(j["counts"].clone())?;
            if sums.len() != counts.len() || sums.is_empty() {
                return Err("league/count coverage".into());
            }
            let sums = sums
                .iter()
                .map(|s| integer(s))
                .collect::<Result<Vec<_>>>()?;
            let total: BigUint = sums.iter().sum();
            let budget = integer(j["budget18"].as_str().ok_or("budget")?)?;
            if total.is_zero() || total.bits() > 136 || budget.bits() > 256 {
                return Err("aggregate/budget range".into());
            }
            let mut y = sums
                .iter()
                .map(|s| u(&(s * 1_000_000u64 / &total).to_string()))
                .collect::<Result<Vec<_>>>()?;
            let used: U256 = y.iter().copied().sum();
            *y.last_mut().unwrap() += U256::from(1_000_000) - used;
            let average = &budget / (&total * 1_000_000u64);
            let maximum = &average * 2u64;
            let preliminary = algorithm::calc_fraction_distribution_fp(
                &y,
                &counts,
                counts.iter().sum::<u64>() as usize,
                u(&average.to_string())?,
                u(&maximum.to_string())?,
            )
            .map_err(|e| format!("Lysis kernel {e:?}"))?;
            let mut fs = preliminary
                .iter()
                .map(|x| integer(&x.to_string()))
                .collect::<Result<Vec<_>>>()?;
            let cost = |fs: &[BigUint]| {
                sums.iter()
                    .zip(fs)
                    .map(|(s, f)| s * f * 1_000_000u64)
                    .sum::<BigUint>()
            };
            let g0 = cost(&fs);
            if g0 > budget {
                for f in &mut fs {
                    *f = &*f * &budget / &g0;
                }
            }
            let g = cost(&fs);
            if g > budget {
                return Err("final fractions exceed fixed18 budget".into());
            }
            json!({"fractions":fs.iter().map(ToString::to_string).collect::<Vec<_>>(),"average6":average.to_string(),"maximum6":maximum.to_string(),"reserved18":g.to_string(),"unused18":(&budget-&g).to_string(),"preliminary18":g0.to_string(),"s":total.to_string(),"fixed18_reconciliation":true})
        }
        _ => return Err("unknown math operation".into()),
    };
    write_json(Path::new(j["out"].as_str().ok_or("out")?), &out)
}
fn main() {
    if let Err(e) = run() {
        eprintln!("Math failed: {e}");
        std::process::exit(1)
    }
}
