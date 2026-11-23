use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use rand_distr::{Distribution, StandardNormal};

pub struct Rotation {
    p: faer::Mat<f64>,
}

impl Rotation {
    pub fn new(dim: usize, seed: u64) -> Self {
        let mut rng = ChaCha20Rng::seed_from_u64(seed);
        Self::generate(dim, &mut rng)
    }

    pub fn from_seed(dim: usize, seed: &[u8; 32]) -> Self {
        let mut rng = ChaCha20Rng::from_seed(*seed);
        Self::generate(dim, &mut rng)
    }

    pub fn matrix(&self) -> &faer::Mat<f64> {
        &self.p
    }

    fn generate(dim: usize, rng: &mut ChaCha20Rng) -> Self {
        let a = faer::Mat::<f64>::from_fn(dim, dim, |_, _| StandardNormal.sample(rng));
        let qr = a.qr();
        let mut q = qr.compute_Q();
        let r = qr.R();
        for j in 0..dim {
            let sign = if r[(j, j)] < 0.0 { -1.0 } else { 1.0 };
            for i in 0..dim {
                q[(i, j)] *= sign;
            }
        }
        Self { p: q }
    }

    pub fn dim(&self) -> usize {
        self.p.nrows()
    }

    pub fn apply_transpose(&self, v: &[f64]) -> Vec<f64> {
        let dim = self.dim();
        assert_eq!(v.len(), dim);
        let mut out = vec![0.0; dim];
        for (j, o) in out.iter_mut().enumerate() {
            let mut s = 0.0;
            for i in 0..dim {
                s += self.p[(i, j)] * v[i];
            }
            *o = s;
        }
        out
    }

    pub fn apply_transpose_rows(&self, rows: &faer::Mat<f64>) -> faer::Mat<f64> {
        let dim = self.dim();
        assert_eq!(rows.ncols(), dim);
        let mut out = faer::Mat::<f64>::zeros(rows.nrows(), dim);
        faer::linalg::matmul::matmul(
            &mut out,
            faer::Accum::Replace,
            rows,
            &self.p,
            1.0,
            faer::Par::Seq,
        );
        out
    }

    pub fn apply(&self, v: &[f64]) -> Vec<f64> {
        let dim = self.dim();
        assert_eq!(v.len(), dim);
        let mut out = vec![0.0; dim];
        for (i, o) in out.iter_mut().enumerate() {
            let mut s = 0.0;
            for j in 0..dim {
                s += self.p[(i, j)] * v[j];
            }
            *o = s;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::Rotation;

    fn gram(p: &Rotation) -> f64 {
        let d = p.dim();
        let mut worst: f64 = 0.0;
        for i in 0..d {
            for j in 0..d {
                let mut s = 0.0;
                for k in 0..d {
                    s += p.p[(k, i)] * p.p[(k, j)];
                }
                let target = if i == j { 1.0 } else { 0.0 };
                worst = worst.max((s - target).abs());
            }
        }
        worst
    }

    #[test]
    fn haar_rotation_is_orthogonal() {
        for dim in [3, 64, 768] {
            let p = Rotation::new(dim, 42);
            assert!(gram(&p) < 1e-10, "dim {dim} off by {}", gram(&p));
        }
    }

    #[test]
    fn transpose_inverts_forward_rotation() {
        let p = Rotation::new(9, 5);
        let v: Vec<f64> = (0..9).map(|i| (i as f64) * 0.25 - 1.0).collect();
        let back = p.apply_transpose(&p.apply(&v));
        for (a, b) in v.iter().zip(back) {
            assert!((a - b).abs() < 1e-12);
        }
    }
}
