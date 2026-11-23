use pqr_core::math::{beta_for, beta_quantile, gd_minus, MathError};

#[test]
fn beta_quantile_matches_external_quadrature() {
    let cases = [
        (3usize, 0.01, 1.413515733350),
        (3, 0.05, 1.396802246667),
        (64, 0.01, 2.303583173890),
        (64, 0.05, 1.646598452781),
        (256, 0.002, 2.863231469425),
        (768, 0.01, 2.324515663286),
    ];
    for (dim, delta, expected) in cases {
        let got = beta_quantile(dim, delta).unwrap();
        assert!(
            (got - expected).abs() < 1e-10,
            "dim={dim} delta={delta}: got {got:.12}, expected {expected:.12}"
        );
    }
}

#[test]
fn beta_quantile_inverts_gd() {
    for dim in [3usize, 64, 768] {
        for delta in [0.01, 0.05] {
            let eps = beta_quantile(dim, delta).unwrap();
            let back = gd_minus(eps, dim);
            assert!(
                (back - delta).abs() < 1e-10,
                "dim={dim} delta={delta}: recovered {back:.12}"
            );
        }
    }
}

#[test]
fn beta_budget_follows_theorem_three() {
    assert!((beta_for(1.0, 10, 0.01).unwrap() - 0.001).abs() < 1e-15);
    assert!((beta_for(0.9, 10, 0.01).unwrap() - 0.001).abs() < 1e-15);
    assert!((beta_for(0.9, 100, 0.01).unwrap() - 0.001).abs() < 1e-15);
    assert!((beta_for(0.8, 10, 0.01).unwrap() - 0.002).abs() < 1e-15);
}

#[test]
fn clopper_pearson_matches_known_values() {
    let zero = pqr_core::audit::clopper_pearson_upper(0, 1000);
    assert!((zero - 0.002991).abs() < 1e-5, "zero {zero}");
    let five = pqr_core::audit::clopper_pearson_upper(5, 1000);
    assert!((five - 0.01052).abs() < 1e-4, "five {five}");
}

#[test]
fn quantile_rejects_delta_below_numerical_tail() {
    assert_eq!(beta_quantile(3, 1e-9).unwrap_err(), MathError::Tail);
}
