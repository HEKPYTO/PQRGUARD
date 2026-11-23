use pqr_core::control::{proposition_one_failures, ControlConfig};
use pqr_core::math::beta_quantile;

fn cfg(k: usize, tau: f64, rotations: u64) -> ControlConfig {
    ControlConfig {
        dim: 64,
        k,
        tau,
        delta: 0.01,
        rotations,
        seed: 7,
        rho: 0.5,
        eta: 1e-6,
    }
}

#[test]
fn positive_control_lands_in_paper_range() {
    let report = proposition_one_failures(&cfg(1, 1.0, 20_000), None).unwrap();
    assert!(
        (0.005..=0.015).contains(&report.rate),
        "exact k=1 rate {:.4} outside 0.5-1.5%",
        report.rate
    );

    let report = proposition_one_failures(&cfg(10, 0.9, 20_000), None).unwrap();
    assert!(
        (0.005..=0.015).contains(&report.rate),
        "tau=0.9 k=10 rate {:.4} outside 0.5-1.5%",
        report.rate
    );
}

#[test]
fn weakened_epsilon_is_detected() {
    let eps_02 = beta_quantile(64, 0.02).unwrap();
    let report = proposition_one_failures(&cfg(1, 1.0, 20_000), Some(eps_02)).unwrap();
    assert!(
        (0.015..=0.025).contains(&report.rate),
        "weakened 0.02 rate {:.4} outside 1.5-2.5%",
        report.rate
    );

    let eps_05 = beta_quantile(64, 0.05).unwrap();
    let report = proposition_one_failures(&cfg(1, 1.0, 20_000), Some(eps_05)).unwrap();
    assert!(
        (0.04..=0.06).contains(&report.rate),
        "weakened 0.05 rate {:.4} outside 4-6%",
        report.rate
    );
}
