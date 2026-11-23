use pqr_core::code::{code_from, prefix_stats};
use pqr_core::math::beta_quantile;
use pqr_core::rotation::Rotation;

#[test]
fn lemma_one_error_tail_matches_beta_law() {
    let dim = 8usize;
    let delta = 0.05;
    let eps = beta_quantile(dim, delta).unwrap();
    let rho = 0.5f64;
    let perp = (1.0 - rho * rho).sqrt();
    let mut o = vec![0.0; dim];
    o[0] = 1.0;
    let mut q = vec![0.0; dim];
    q[0] = rho;
    q[1] = perp;

    let trials = 30_000usize;
    let mut tail = 0usize;
    for seed in 0..trials as u64 {
        let rotation = Rotation::new(dim, seed);
        let o_rot = rotation.apply_transpose(&o);
        let q_rot = rotation.apply_transpose(&q);
        let code = code_from(&o_rot);
        let stats = prefix_stats(&code, &o_rot, &q_rot, dim).expect("positive overlap");
        let zeta = (stats.rho_hat - rho) / (perp * stats.kappa);
        if zeta < -eps {
            tail += 1;
        }
    }
    let rate = tail as f64 / trials as f64;
    assert!(
        (rate - delta).abs() < 0.007,
        "tail {rate:.4}, expected {delta}"
    );
}
