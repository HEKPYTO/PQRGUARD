#[derive(Debug, Clone, Copy)]
pub struct PrefixStats {
    pub rho_hat: f64,
    pub kappa: f64,
    pub a: f64,
}

pub fn code_from(o_rot: &[f64]) -> Vec<i8> {
    o_rot
        .iter()
        .map(|x| if *x < 0.0 { -1 } else { 1 })
        .collect()
}

pub fn prefix_stats(code: &[i8], o_rot: &[f64], q_rot: &[f64], m: usize) -> Option<PrefixStats> {
    let dim = code.len();
    if m == 0 || m > dim || o_rot.len() != dim || q_rot.len() != dim {
        return None;
    }
    let inv_sqrt = 1.0 / (dim as f64).sqrt();
    let mut a = 0.0;
    let mut b = 0.0;
    for i in 0..m {
        a += code[i] as f64 * o_rot[i];
        b += code[i] as f64 * q_rot[i];
    }
    a *= inv_sqrt;
    b *= inv_sqrt;
    if a <= 0.0 {
        return None;
    }
    let spread = (m as f64 / dim as f64 - a * a).max(0.0).sqrt();
    let kappa = spread / (a * ((dim - 1) as f64).sqrt());
    Some(PrefixStats {
        rho_hat: b / a,
        kappa,
        a,
    })
}

pub fn prune(stats: &PrefixStats, theta: f64, slack: f64, eps0: f64) -> bool {
    stats.rho_hat + slack * stats.kappa * eps0 < theta
}
