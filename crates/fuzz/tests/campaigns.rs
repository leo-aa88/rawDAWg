//! Fuzz regressions and a short smoke campaign per target (CI-sized). Long campaigns run
//! with `cargo xtask fuzz`; every input that ever failed is kept in
//! `fixtures/fuzz-regressions/<target>/` and must keep passing.

use std::path::Path;
use std::time::Duration;

fn regressions_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/fuzz-regressions")
}

#[test]
fn every_saved_regression_input_passes() {
    let mut replayed = 0;
    for dir in std::fs::read_dir(regressions_dir()).expect("regressions folder").flatten() {
        let name = dir.file_name().to_string_lossy().into_owned();
        let target = rawdawg_fuzz::targets::by_name(&name).unwrap_or_else(|| panic!("regression folder {name} names no target"));
        for f in std::fs::read_dir(dir.path()).unwrap().flatten() {
            let input = std::fs::read(f.path()).unwrap();
            if let Some(msg) = rawdawg_fuzz::replay(&target, &input) {
                panic!("{name}: {} fails again: {msg}", f.path().display());
            }
            replayed += 1;
        }
    }
    assert!(replayed >= 4, "expected the committed regression inputs, found {replayed}");
}

#[test]
fn every_target_survives_a_short_seeded_campaign() {
    let out = std::env::temp_dir().join(format!("rawdawg-fuzz-smoke-{}", std::process::id()));
    let mut failures = Vec::new();
    for t in rawdawg_fuzz::targets::all() {
        assert!(!t.seeds.is_empty(), "{} has no seeds", t.name);
        let r = rawdawg_fuzz::run_campaign(&t, 300, 42, Duration::from_secs(20), &out, 4, Duration::from_secs(30));
        assert_eq!(r.accepted_seeds, t.seeds.len(), "{}: every seed must be accepted", t.name);
        for f in r.findings {
            failures.push(format!("{} iteration {}: {} ({})", f.target, f.iteration, f.message, f.input.display()));
        }
    }
    let _ = std::fs::remove_dir_all(&out);
    assert!(failures.is_empty(), "fuzz findings:\n{}", failures.join("\n"));
}

#[test]
fn mutators_are_deterministic() {
    use rawdawg_fuzz::mutate::{Rng, mutate_bytes, mutate_json};
    let seed = b"MThd\0\0\0\x06\0\x01\0\x02\x01\xe0".to_vec();
    let doc = serde_json::json!({ "a": [1, 2, { "b": "c" }], "d": 0.5 });
    let run = || {
        let mut rng = Rng::new(9);
        (0..50).map(|_| (mutate_bytes(&mut rng, &seed, &[], 1024), mutate_json(&mut rng, &doc))).collect::<Vec<_>>()
    };
    assert_eq!(run(), run(), "same seed, same inputs: findings reproduce from (target, seed, iteration)");
}
