use pqr_core::control::{proposition_one_failures, ControlConfig};
use pqr_core::math::beta_quantile;

fn main() {
    let cases = [
        (64usize, 1usize, 1.0f64, 100_000u64),
        (64, 10, 0.9, 100_000),
        (64, 10, 1.0, 100_000),
        (256, 100, 1.0, 10_000),
    ];
    for (dim, k, tau, rotations) in cases {
        let cfg = ControlConfig {
            dim,
            k,
            tau,
            delta: 0.01,
            rotations,
            seed: 11,
            rho: 0.5,
            eta: 1e-6,
        };
        let r = proposition_one_failures(&cfg, None).unwrap();
        println!(
            "D={dim} k={k} tau={tau} rotations={rotations} failures={} rate={:.5}",
            r.failures, r.rate
        );
    }
    for (delta, name) in [(0.02, "0.02"), (0.05, "0.05")] {
        let cfg = ControlConfig {
            dim: 64,
            k: 1,
            tau: 1.0,
            delta: 0.01,
            rotations: 100_000,
            seed: 11,
            rho: 0.5,
            eta: 1e-6,
        };
        let eps = beta_quantile(64, delta).unwrap();
        let r = proposition_one_failures(&cfg, Some(eps)).unwrap();
        println!(
            "weakened delta={name} failures={} rate={:.5}",
            r.failures, r.rate
        );
    }
}
