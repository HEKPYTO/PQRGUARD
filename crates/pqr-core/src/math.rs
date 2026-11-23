use std::f64::consts::PI;

use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum MathError {
    #[error("dimension must be at least 3")]
    Dimension,
    #[error("delta must be in (0, 0.5]")]
    Delta,
    #[error("failure budget exceeds one half")]
    Budget,
    #[error("delta is below the numerical resolution of the Beta tail")]
    Tail,
    #[error("tau must be in (0, 1]")]
    Tau,
    #[error("k must be positive")]
    K,
}

pub fn gd_minus(s: f64, dim: usize) -> f64 {
    if dim < 3 || s < 0.0 || s.is_nan() {
        return f64::NAN;
    }
    let x = s * s / (dim - 1) as f64;
    0.5 * (1.0 - beta_regularized(0.5, (dim - 2) as f64 / 2.0, x))
}

pub fn beta_quantile(dim: usize, delta: f64) -> Result<f64, MathError> {
    if dim < 3 {
        return Err(MathError::Dimension);
    }
    if !(delta > 0.0 && delta <= 0.5) {
        return Err(MathError::Delta);
    }
    let mut lo = 0.0;
    let mut hi = ((dim - 1) as f64).sqrt() * (1.0 - 1e-15);
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        if gd_minus(mid, dim) > delta {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let eps = hi;
    if gd_minus(eps, dim) > delta + 1e-12 {
        return Err(MathError::Tail);
    }
    Ok(eps)
}

pub fn misses_allowed(tau: f64, k: usize) -> usize {
    if tau >= 1.0 || k == 0 {
        0
    } else {
        (((1.0 - tau) * k as f64).floor() as usize).min(k - 1)
    }
}

pub fn beta_for(tau: f64, k: usize, delta: f64) -> Result<f64, MathError> {
    if !(tau > 0.0 && tau <= 1.0) {
        return Err(MathError::Tau);
    }
    if k == 0 {
        return Err(MathError::K);
    }
    if !(delta > 0.0 && delta < 1.0) {
        return Err(MathError::Delta);
    }
    let m = misses_allowed(tau, k);
    let beta = delta * (m + 1) as f64 / k as f64;
    if beta > 0.5 {
        return Err(MathError::Budget);
    }
    Ok(beta)
}

pub fn beta_cdf(a: f64, b: f64, x: f64) -> f64 {
    beta_regularized(a, b, x)
}

pub fn beta_quantile_general(a: f64, b: f64, p: f64) -> Result<f64, MathError> {
    if !(a > 0.0 && b > 0.0) || !(p > 0.0 && p < 1.0) {
        return Err(MathError::Delta);
    }
    let mut lo = 0.0;
    let mut hi = 1.0;
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        if beta_regularized(a, b, mid) < p {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Ok(0.5 * (lo + hi))
}

fn beta_regularized(a: f64, b: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let front =
        (ln_gamma(a + b) - ln_gamma(a) - ln_gamma(b) + a * x.ln() + b * (1.0 - x).ln()).exp();
    if x < (a + 1.0) / (a + b + 2.0) {
        front * beta_continued_fraction(a, b, x) / a
    } else {
        1.0 - front * beta_continued_fraction(b, a, 1.0 - x) / b
    }
}

fn beta_continued_fraction(a: f64, b: f64, x: f64) -> f64 {
    const TINY: f64 = 1e-300;
    const EPS: f64 = 3e-16;
    let qab = a + b;
    let qap = a + 1.0;
    let qam = a - 1.0;
    let mut c = 1.0;
    let mut d = 1.0 - qab * x / qap;
    if d.abs() < TINY {
        d = TINY;
    }
    d = 1.0 / d;
    let mut h = d;
    for m in 1..=400 {
        let m = m as f64;
        let m2 = 2.0 * m;
        let aa = m * (b - m) * x / ((qam + m2) * (a + m2));
        d = 1.0 + aa * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + aa / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        h *= d * c;
        let aa = -(a + m) * (qab + m) * x / ((a + m2) * (qap + m2));
        d = 1.0 + aa * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + aa / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        let delta = d * c;
        h *= delta;
        if (delta - 1.0).abs() < EPS {
            break;
        }
    }
    h
}

fn ln_gamma(z: f64) -> f64 {
    const C: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    if z < 0.5 {
        return (PI / (PI * z).sin()).ln() - ln_gamma(1.0 - z);
    }
    let z = z - 1.0;
    let mut x = C[0];
    for (i, c) in C.iter().enumerate().skip(1) {
        x += c / (z + i as f64);
    }
    let t = z + 7.5;
    0.5 * (2.0 * PI).ln() + (z + 0.5) * t.ln() - t + x.ln()
}
