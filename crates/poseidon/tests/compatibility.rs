#![allow(clippy::print_stdout)] // opt-in performance measurements

use ark_bn254::Fr;
use ark_ff::{BigInteger, PrimeField};
use outbe_poseidon::{Poseidon2, PoseidonHasher};

// Captured on 2026-10-14 from the pre-migration Rust implementation, using
// cargo test --locked --release -p outbe-poseidon --test compatibility.
// Rows: sequential 1..n, all zeroes, repeating p-1/p-2/p-3. Columns: 0..9,25.
const FIXTURES: [[&str; 11]; 3] = [
    [
        "11250791130336988991462250958918728798886439319225016858543557054782819955502",
        "10190015755989328289879378487807721086446093622177241109507523918927702106995",
        "1594597865669602199208529098208508950092942746041644072252494753744672355203",
        "10385265492659894228182953993950191188438222933552541797453614238219982629234",
        "8615049788434614272061777381929479688528564767750167561409097996914085376441",
        "15505397346924486196327203107887014587518275791483664580681206942555362712027",
        "2105002305651931879002818140657459039449884062689019891434313100129560805726",
        "10391115629414681976188035788285407615853840201595689384030320669823070294584",
        "845892602229388737959657982733116293377451109923813426052567128267389432130",
        "16692124890087128654872179168691914716060622521399447422899829834653452210502",
        "21435801421032998087123511242900982332213795770542300261242971986425126741559",
    ],
    [
        "11250791130336988991462250958918728798886439319225016858543557054782819955502",
        "17668610518173883319035856328661308815933580113901672897605691629848497347345",
        "5151499478991301833156025595048985053689893395646836724335623777508747990769",
        "19327780213786520351830514178837841101936963116736649396547512961449185189380",
        "14809712463008457233891043966654826926909229366582488672652099303846753427764",
        "6934580912915885971773338843820653588133858608884985930823699956582566673272",
        "12634194061379009108759804878757214728589001555284212772464618674518800069074",
        "5697520257009963651067629323921478072275995991882428529159246439869378448840",
        "273490850853590408366637299747287336099076347079291197056428461674499681621",
        "987985619119086124193192641085545186075269149214241125447667568366448533950",
        "15904935540352533780238270824826310231685630286635798279511796614553131806989",
    ],
    [
        "11250791130336988991462250958918728798886439319225016858543557054782819955502",
        "19229846669218739769030580929996973050110488644770850861910889092883005202156",
        "19101290737387150201794024388203946677276484422241057801127946814433772113065",
        "763457274310296358225980576486454331006951543916031555767408828959340805020",
        "14449265229846954092848081742099984315559712890910138824850906162804707944190",
        "9914397840619922576418278075422977763930452202087650645588940498044098857233",
        "12372877862237622941300234257570969207337651179866009513903815597124263318553",
        "12835418301521300320222679241260765877047137460732815892218272617562344348921",
        "21425658203540418077013869824648329825292072486274348262034251049748415039485",
        "14549240563097245599783240340635661620644602089260328872751573849182124644529",
        "20052326416027867128131156308893035624004989308475010083776766687891753797845",
    ],
];

fn check_fixtures() {
    for (kind, row) in FIXTURES.iter().enumerate() {
        for (column, len) in (0..=9).chain([25]).enumerate() {
            let inputs: Vec<Fr> = (0..len)
                .map(|i| match kind {
                    0 => Fr::from(i as u64 + 1),
                    1 => Fr::from(0u64),
                    _ => -Fr::from((i % 3 + 1) as u64),
                })
                .collect();
            assert_eq!(
                Poseidon2::new().hash(&inputs).unwrap(),
                row[column].parse::<Fr>().unwrap(),
                "kind={kind}, len={len}"
            );
        }
    }
}

#[test]
fn frozen_compatibility_fixtures() {
    check_fixtures();
}

#[test]
fn failed_native_command_releases_gate_and_clears_dispatcher_error() {
    // Pinned bbapi_chonk.cpp explicitly rejects Stop before Start, without
    // touching SRS, proof data or a running service.
    let result = outbe_barretenberg::with_api(|api| {
        api.chonk_batch_verifier_stop()
            .map_err(|error| outbe_barretenberg::RuntimeError::Backend(std::sync::Arc::new(error)))
    });
    assert!(matches!(
        result,
        Err(outbe_barretenberg::RuntimeError::Backend(_))
    ));
    check_fixtures();
}

#[cfg(feature = "test-utils")]
#[test]
fn injection_is_scoped_and_thread_local() {
    let input = [Fr::from(1u64), Fr::from(2u64)];
    let expected = Poseidon2::new().hash(&input).unwrap();
    {
        let _guard = outbe_poseidon::testing::fail_after(1);
        assert_eq!(Poseidon2::new().hash(&input).unwrap(), expected);
        assert!(matches!(
            Poseidon2::new().hash(&input).unwrap_err(),
            outbe_poseidon::PoseidonError::Runtime(_)
        ));
        std::thread::scope(|scope| {
            scope.spawn(|| assert_eq!(Poseidon2::new().hash(&input).unwrap(), expected));
        });
    }
    assert_eq!(Poseidon2::new().hash(&input).unwrap(), expected);
}

#[test]
fn parallel_hashing_is_deterministic() {
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(check_fixtures);
        }
    });
}

#[test]
fn rate_boundaries_differ_from_upstream_standard_hash() {
    for len in [3, 6] {
        let inputs: Vec<Fr> = (1..=len).map(Fr::from).collect();
        let compatible = Poseidon2::new().hash(&inputs).unwrap();
        let standard = outbe_barretenberg::with_api(|api| {
            api.poseidon2_hash(
                inputs
                    .iter()
                    .map(|x| x.into_bigint().to_bytes_be())
                    .collect(),
            )
            .map_err(|e| outbe_barretenberg::RuntimeError::Backend(std::sync::Arc::new(e)))
        })
        .unwrap();
        assert_ne!(compatible, Fr::from_be_bytes_mod_order(&standard.hash));
        assert_eq!(compatible, FIXTURES[0][len as usize].parse::<Fr>().unwrap());
    }
}

#[test]
#[ignore = "release performance measurement"]
fn measure_hashing() {
    // Darwin arm64, 2026-10-14: pre-migration 100k hashes 776.048 ms;
    // native 884.236-908.874 ms. These observations are not acceptance targets.
    let input = [Fr::from(1u64), Fr::from(2u64)];
    let start = std::time::Instant::now();
    for _ in 0..100_000 {
        std::hint::black_box(Poseidon2::new().hash(std::hint::black_box(&input)).unwrap());
    }
    println!("100000 two-input hashes: {:?}", start.elapsed());

    // Run the same binding encode/decode path against a fixed in-memory
    // response to isolate MessagePack overhead from native computation.
    use barretenberg_rs::generated_types::{Poseidon2PermutationResponse, Response};
    use barretenberg_rs::{api::BarretenbergApi, backend::Backend, error::BarretenbergError};
    struct EchoResponse(Vec<u8>);
    impl Backend for EchoResponse {
        fn call(&mut self, _: &[u8]) -> Result<Vec<u8>, BarretenbergError> {
            Ok(self.0.clone())
        }
        fn destroy(&mut self) -> Result<(), BarretenbergError> {
            Ok(())
        }
    }
    let response = Response::Poseidon2PermutationResponse(Poseidon2PermutationResponse {
        outputs: std::array::from_fn(|_| vec![0; 32]),
    });
    let mut api = BarretenbergApi::new(EchoResponse(rmp_serde::to_vec_named(&response).unwrap()));
    let state = [
        Fr::from(1u64),
        Fr::from(2u64),
        Fr::from(0u64),
        Fr::from(2u128 << 64),
    ];
    let start = std::time::Instant::now();
    for _ in 0..100_000 {
        std::hint::black_box(
            api.poseidon2_permutation(state.map(|x| x.into_bigint().to_bytes_be()))
                .unwrap(),
        );
    }
    println!(
        "100000 binding-only encode/decode calls: {:?}",
        start.elapsed()
    );

    let start = std::time::Instant::now();
    for _ in 0..100_000 {
        outbe_barretenberg::with_api(|_| Ok::<_, outbe_barretenberg::RuntimeError>(())).unwrap();
    }
    println!("100000 uncontended gate/API scopes: {:?}", start.elapsed());
}
