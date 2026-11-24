use std::fs;
use std::io;
use std::path::Path;

use rand::Rng;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::certificate::{Certificate, Degraded, DegradedReason, Target};
use crate::math::{beta_for, beta_quantile, MathError};
use crate::rotation::Rotation;

const MANIFEST: &str = "manifest.json";
const VECTORS: &str = "vectors.f32";
const CENTROIDS: &str = "centroids.f32";
const ASSIGN: &str = "assign.u32";
const NORMS: &str = "norms.f32";
const CODES: &str = "codes.u64";
const COEFFS: &str = "coeffs.f64";
const COEFF_C: &str = "coeffc.f64";
const ORDER: &str = "order.u32";
const STARTS: &str = "starts.u32";

#[derive(Debug, Clone)]
pub struct BuildConfig {
    pub partitions: usize,
    pub stages: Vec<usize>,
    pub iterations: usize,
}

impl Default for BuildConfig {
    fn default() -> Self {
        Self {
            partitions: 0,
            stages: Vec::new(),
            iterations: 10,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildReceipt {
    pub index_id: String,
    pub dim: usize,
    pub n: usize,
    pub partitions: usize,
    pub stages: Vec<usize>,
    pub seed_commitment: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    pub id: u64,
    pub distance2: f64,
}

#[derive(Debug, Clone)]
pub enum QueryOutcome {
    Certified {
        hits: Vec<Hit>,
        certificate: Certificate,
    },
    Degraded {
        hits: Vec<Hit>,
        degraded: Degraded,
    },
}

#[derive(Debug, Clone)]
pub struct QueryOptions {
    pub target: Target,
    pub nprobe: usize,
    pub reasons: Vec<DegradedReason>,
    pub stage_eps: Option<Vec<f64>>,
}

#[derive(Debug, Error)]
pub enum IndexError {
    #[error("dimension must be at least 3")]
    Dimension,
    #[error("vectors must be a nonempty multiple of dim")]
    Vectors,
    #[error("non-finite value in vectors")]
    NonFinite,
    #[error("k exceeds live vectors")]
    KExceedsLive,
    #[error("nprobe must be positive")]
    Nprobe,
    #[error("stage bits must be strictly increasing and within dim")]
    Stages,
    #[error(transparent)]
    Math(#[from] MathError),
    #[error("index checksum mismatch")]
    Checksum,
    #[error("seed does not match index commitment")]
    SeedMismatch,
    #[error("corrupt index: {0}")]
    Corrupt(String),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

struct FastStage {
    tables: Vec<[f64; 256]>,
}

impl FastStage {
    fn new(q: &[f64], m: usize) -> Self {
        let bytes = m.div_ceil(8);
        let mut tables = vec![[0f64; 256]; bytes];
        for (byte, table) in tables.iter_mut().enumerate() {
            for (pattern, slot) in table.iter_mut().enumerate() {
                let mut sum = 0.0;
                for bit in 0..8 {
                    let idx = byte * 8 + bit;
                    if idx >= m {
                        break;
                    }
                    let sign = if (pattern >> bit) & 1 == 1 { 1.0 } else { -1.0 };
                    sum += sign * q[idx];
                }
                *slot = sum;
            }
        }
        Self { tables }
    }

    fn dot(&self, codes: &[u64], base: usize) -> f64 {
        let mut s0 = 0.0f64;
        let mut s1 = 0.0f64;
        let mut s2 = 0.0f64;
        let mut s3 = 0.0f64;
        let mut s4 = 0.0f64;
        let mut s5 = 0.0f64;
        let mut s6 = 0.0f64;
        let mut s7 = 0.0f64;
        let mut chunks = self.tables.chunks_exact(8);
        for (ci, chunk) in chunks.by_ref().enumerate() {
            let word = codes[base + ci];
            s0 += chunk[0][(word & 0xff) as usize];
            s1 += chunk[1][((word >> 8) & 0xff) as usize];
            s2 += chunk[2][((word >> 16) & 0xff) as usize];
            s3 += chunk[3][((word >> 24) & 0xff) as usize];
            s4 += chunk[4][((word >> 32) & 0xff) as usize];
            s5 += chunk[5][((word >> 40) & 0xff) as usize];
            s6 += chunk[6][((word >> 48) & 0xff) as usize];
            s7 += chunk[7][((word >> 56) & 0xff) as usize];
        }
        let done = self.tables.len() - chunks.remainder().len();
        for (i, table) in chunks.remainder().iter().enumerate() {
            let byte = done + i;
            let word = codes[base + byte / 8];
            s0 += table[((word >> ((byte % 8) * 8)) & 0xff) as usize];
        }
        ((s0 + s1) + (s2 + s3)) + ((s4 + s5) + (s6 + s7))
    }
}

pub struct PqrIndex {
    dim: usize,
    n: usize,
    partitions: usize,
    stages: Vec<usize>,
    words: usize,
    vectors: Vec<f32>,
    centroids: Vec<f32>,
    assign: Vec<u32>,
    order: Vec<u32>,
    starts: Vec<u32>,
    norms: Vec<f32>,
    codes: Vec<u64>,
    coeff_a: Vec<f64>,
    coeff_k: Vec<f64>,
    coeff_c: Vec<f64>,
    scan_norms: Vec<f32>,
    scan_codes: Vec<u64>,
    scan_a: Vec<f64>,
    scan_k: Vec<f64>,
    scan_c: Vec<f64>,
    scan_ids: Vec<u32>,
    rotation: Rotation,
    seed_commitment: String,
    index_id: String,
}

impl PqrIndex {
    pub fn build(
        vectors: &[f32],
        dim: usize,
        cfg: &BuildConfig,
        seed: &[u8; 32],
    ) -> Result<(Self, BuildReceipt), IndexError> {
        if dim < 3 {
            return Err(IndexError::Dimension);
        }
        if vectors.is_empty() || vectors.len() % dim != 0 {
            return Err(IndexError::Vectors);
        }
        if vectors.iter().any(|v| !v.is_finite()) {
            return Err(IndexError::NonFinite);
        }
        let n = vectors.len() / dim;
        let partitions = if cfg.partitions == 0 {
            (n as f64).sqrt().ceil() as usize
        } else {
            cfg.partitions.min(n)
        };
        let partitions = partitions.max(1);
        let stages = if cfg.stages.is_empty() {
            default_stages(dim)
        } else {
            cfg.stages.clone()
        };
        if stages.is_empty()
            || stages.iter().any(|m| *m == 0 || *m > dim)
            || stages.windows(2).any(|w| w[0] >= w[1])
        {
            return Err(IndexError::Stages);
        }

        let mut rng = ChaCha20Rng::from_seed(*seed);
        let (centroids, assign) = kmeans(vectors, n, dim, partitions, cfg.iterations, &mut rng);
        let (order, starts) = partition_order(&assign, partitions);
        let stages_len = stages.len();
        let words = dim.div_ceil(64);
        let mut index = Self {
            dim,
            n,
            partitions,
            stages,
            words,
            vectors: vectors.to_vec(),
            centroids,
            assign,
            order,
            starts,
            norms: vec![0f32; n],
            codes: vec![0u64; n * words],
            coeff_a: vec![0f64; n * stages_len],
            coeff_k: vec![0f64; n * stages_len],
            coeff_c: vec![0f64; n * stages_len],
            scan_norms: Vec::new(),
            scan_codes: Vec::new(),
            scan_a: Vec::new(),
            scan_k: Vec::new(),
            scan_c: Vec::new(),
            scan_ids: Vec::new(),
            rotation: Rotation::from_seed(dim, seed),
            seed_commitment: hex(&blake3::hash(seed)),
            index_id: String::new(),
        };
        index.encode_all();
        if index.norms.iter().any(|v| !v.is_finite()) {
            return Err(IndexError::NonFinite);
        }
        index.build_scan_layout();
        index.index_id = index.compute_id();
        let receipt = BuildReceipt {
            index_id: index.index_id.clone(),
            dim: index.dim,
            n: index.n,
            partitions: index.partitions,
            stages: index.stages.clone(),
            seed_commitment: index.seed_commitment.clone(),
        };
        Ok((index, receipt))
    }

    pub fn reencode(&mut self, seed: &[u8; 32]) {
        self.rotation = Rotation::from_seed(self.dim, seed);
        self.seed_commitment = hex(&blake3::hash(seed));
        self.encode_all();
        self.build_scan_layout();
        self.index_id = self.compute_id();
    }

    fn encode_all(&mut self) {
        let Self {
            dim,
            n,
            partitions,
            stages,
            words,
            vectors,
            centroids,
            assign,
            order: _,
            starts: _,
            norms,
            codes,
            coeff_a,
            coeff_k,
            coeff_c,
            rotation,
            ..
        } = self;
        let stages_len = stages.len();
        norms.fill(0.0);
        codes.fill(0);
        coeff_a.fill(0.0);
        coeff_k.fill(0.0);
        coeff_c.fill(0.0);
        let mut centroid_rot: Vec<Vec<f64>> = Vec::with_capacity(*partitions);
        for p in 0..*partitions {
            let c: Vec<f64> = (0..*dim).map(|j| centroids[p * *dim + j] as f64).collect();
            centroid_rot.push(rotation.apply_transpose(&c));
        }
        let inv_sqrt = 1.0 / (*dim as f64).sqrt();
        let chunk = 512usize;
        let mut residuals = faer::Mat::<f64>::zeros(chunk.min(*n), *dim);
        let mut rotated = faer::Mat::<f64>::zeros(chunk.min(*n), *dim);
        let mut start = 0usize;
        while start < *n {
            let rows = chunk.min(*n - start);
            for r in 0..rows {
                let id = start + r;
                let c = assign[id] as usize;
                let mut x2 = 0.0f64;
                for j in 0..*dim {
                    let d = vectors[id * *dim + j] as f64 - centroids[c * *dim + j] as f64;
                    residuals[(r, j)] = d;
                    x2 += d * d;
                }
                let x = x2.sqrt();
                norms[id] = x as f32;
                if x > 0.0 {
                    let inv = 1.0 / x;
                    for j in 0..*dim {
                        residuals[(r, j)] *= inv;
                    }
                }
            }
            faer::linalg::matmul::matmul(
                &mut rotated.subrows_mut(0, rows),
                faer::Accum::Replace,
                residuals.subrows(0, rows),
                rotation.matrix(),
                1.0,
                faer::Par::Seq,
            );
            for r in 0..rows {
                let id = start + r;
                if norms[id] == 0.0 {
                    continue;
                }
                let c = assign[id] as usize;
                let base = id * *words;
                for j in 0..*dim {
                    if rotated[(r, j)] >= 0.0 {
                        codes[base + j / 64] |= 1u64 << (j % 64);
                    }
                }
                for (l, m) in stages.iter().enumerate() {
                    let mut a = 0.0f64;
                    for j in 0..*m {
                        let bit = (codes[base + j / 64] >> (j % 64)) & 1;
                        let s = if bit == 1 { 1.0 } else { -1.0 };
                        a += s * rotated[(r, j)];
                    }
                    a *= inv_sqrt;
                    coeff_a[id * stages_len + l] = a;
                    if a > 0.0 {
                        let spread = (*m as f64 / *dim as f64 - a * a).max(0.0).sqrt();
                        coeff_k[id * stages_len + l] = spread / (a * ((*dim - 1) as f64).sqrt());
                    }
                    let mut ct = 0.0f64;
                    for j in 0..*m {
                        let bit = (codes[base + j / 64] >> (j % 64)) & 1;
                        let s = if bit == 1 { 1.0 } else { -1.0 };
                        ct += s * centroid_rot[c][j];
                    }
                    coeff_c[id * stages_len + l] = ct * inv_sqrt;
                }
            }
            start += rows;
        }
    }

    fn build_scan_layout(&mut self) {
        let n = self.n;
        let words = self.words;
        let shares = self.stages.len();
        let mut norms = vec![0f32; n];
        let mut codes = vec![0u64; n * words];
        let mut a = vec![0f64; n * shares];
        let mut k = vec![0f64; n * shares];
        let mut c = vec![0f64; n * shares];
        for (slot, &idu) in self.order.iter().enumerate() {
            let id = idu as usize;
            norms[slot] = self.norms[id];
            codes[slot * words..(slot + 1) * words]
                .copy_from_slice(&self.codes[id * words..(id + 1) * words]);
            for l in 0..shares {
                a[slot * shares + l] = self.coeff_a[id * shares + l];
                k[slot * shares + l] = self.coeff_k[id * shares + l];
                c[slot * shares + l] = self.coeff_c[id * shares + l];
            }
        }
        let mut sorted_norms = vec![0f32; n];
        let mut sorted_codes = vec![0u64; n * words];
        let mut sorted_a = vec![0f64; n * shares];
        let mut sorted_k = vec![0f64; n * shares];
        let mut sorted_c = vec![0f64; n * shares];
        let mut ids = vec![0u32; n];
        for p in 0..self.partitions {
            let begin = self.starts[p] as usize;
            let end = self.starts[p + 1] as usize;
            let mut slots: Vec<usize> = (begin..end).collect();
            slots.sort_unstable_by(|&a, &b| {
                norms[a]
                    .total_cmp(&norms[b])
                    .then_with(|| self.order[a].cmp(&self.order[b]))
            });
            for (offset, &src) in slots.iter().enumerate() {
                let dst = begin + offset;
                sorted_norms[dst] = norms[src];
                sorted_codes[dst * words..(dst + 1) * words]
                    .copy_from_slice(&codes[src * words..(src + 1) * words]);
                for l in 0..shares {
                    sorted_a[dst * shares + l] = a[src * shares + l];
                    sorted_k[dst * shares + l] = k[src * shares + l];
                    sorted_c[dst * shares + l] = c[src * shares + l];
                }
                ids[dst] = self.order[src];
            }
        }
        self.scan_norms = sorted_norms;
        self.scan_codes = sorted_codes;
        self.scan_a = sorted_a;
        self.scan_k = sorted_k;
        self.scan_c = sorted_c;
        self.scan_ids = ids;
    }

    pub fn query(&self, q: &[f32], opts: &QueryOptions) -> Result<QueryOutcome, IndexError> {
        if q.len() != self.dim {
            return Err(IndexError::Dimension);
        }
        if q.iter().any(|v| !v.is_finite()) {
            return Err(IndexError::NonFinite);
        }
        if opts.target.k > self.n {
            return Err(IndexError::KExceedsLive);
        }
        if opts.nprobe == 0 {
            return Err(IndexError::Nprobe);
        }
        let k = opts.target.k;
        let q64: Vec<f64> = q.iter().map(|v| *v as f64).collect();
        let q_rot = self.rotation.apply_transpose(&q64);

        let mut phi2 = vec![0f64; self.partitions];
        for p in 0..self.partitions {
            let mut s = 0.0;
            for j in 0..self.dim {
                let d = q64[j] - self.centroids[p * self.dim + j] as f64;
                s += d * d;
            }
            phi2[p] = s;
        }
        let mut order: Vec<usize> = (0..self.partitions).collect();
        order.sort_unstable_by(|a, b| phi2[*a].total_cmp(&phi2[*b]));
        let visited = opts.nprobe.min(self.partitions);

        let mut candidates: Vec<(f64, u32)> = Vec::with_capacity(k + 1);
        let mut is_visited = vec![false; self.partitions];
        for p in order.iter().take(visited) {
            is_visited[*p] = true;
        }
        for p in order.iter().take(visited) {
            let slice = &self.order[self.starts[*p] as usize..self.starts[*p + 1] as usize];
            for &idu in slice {
                let id = idu as usize;
                let d = exact_distance2(&self.vectors, id, self.dim, &q64);
                push_candidate(&mut candidates, d, id as u32, k);
            }
        }
        let mut r2 = if candidates.len() >= k {
            candidates[k - 1].0
        } else {
            f64::INFINITY
        };
        let mut r = r2.sqrt();
        let shares = self.stages.len();
        let eps = match &opts.stage_eps {
            Some(values) if values.len() == shares => values.clone(),
            _ => self.stage_eps(&opts.target)?,
        };
        let inv_sqrt = 1.0 / (self.dim as f64).sqrt();
        let fast: Vec<FastStage> = self
            .stages
            .iter()
            .map(|m| FastStage::new(&q_rot, *m))
            .collect();

        for p in 0..self.partitions {
            if is_visited[p] {
                continue;
            }
            let phi = phi2[p].sqrt();
            let begin = self.starts[p] as usize;
            let end = self.starts[p + 1] as usize;
            let (lo, hi) = if r2.is_finite() {
                let sub = &self.scan_norms[begin..end];
                let low = begin + sub.partition_point(|x| (*x as f64) < next_down(phi - r));
                let high = begin + sub.partition_point(|x| (*x as f64) <= next_up(phi + r));
                (low, high)
            } else {
                (begin, end)
            };
            for slot in lo..hi {
                let id = self.scan_ids[slot] as usize;
                let x = self.scan_norms[slot] as f64;
                if r2.is_finite() && (x - phi).abs() > r {
                    continue;
                }
                let need_exact = x == 0.0 || phi == 0.0 || !r2.is_finite();
                if need_exact {
                    let d = exact_distance2(&self.vectors, id, self.dim, &q64);
                    push_candidate(&mut candidates, d, id as u32, k);
                    if candidates.len() >= k {
                        r2 = candidates[k - 1].0;
                        r = r2.sqrt();
                    }
                    continue;
                }
                let base = slot * self.words;
                let cbase = slot * shares;
                let theta = (x * x + phi * phi - r * r) / (2.0 * x * phi);
                let mut pruned = false;
                for l in 0..shares {
                    let a = self.scan_a[cbase + l];
                    if a <= 0.0 {
                        continue;
                    }
                    let dot = fast[l].dot(&self.scan_codes, base);
                    let mut b = dot * inv_sqrt;
                    b -= self.scan_c[cbase + l];
                    b /= phi;
                    let kappa = self.scan_k[cbase + l];
                    let slack =
                        (1.0 - theta.max(0.0) * theta.max(0.0)).sqrt() * (1.0 + 1e-9) + 1e-12;
                    if b / a + slack * kappa * eps[l] < theta {
                        pruned = true;
                        break;
                    }
                }
                if pruned {
                    continue;
                }
                let d = exact_distance2(&self.vectors, id, self.dim, &q64);
                push_candidate(&mut candidates, d, id as u32, k);
                r2 = candidates[k - 1].0;
                r = r2.sqrt();
            }
        }

        let hits: Vec<Hit> = candidates
            .iter()
            .map(|(d, id)| Hit {
                id: *id as u64,
                distance2: *d,
            })
            .collect();
        if opts.reasons.is_empty() {
            Ok(QueryOutcome::Certified {
                hits,
                certificate: Certificate::certified(
                    &self.index_id,
                    self.dim,
                    &opts.target,
                    &self.stages,
                ),
            })
        } else {
            Ok(QueryOutcome::Degraded {
                hits,
                degraded: Degraded::new(
                    &self.index_id,
                    self.dim,
                    &opts.target,
                    opts.reasons.clone(),
                ),
            })
        }
    }

    pub fn stage_eps(&self, target: &Target) -> Result<Vec<f64>, IndexError> {
        let beta = beta_for(target.tau, target.k, target.delta)?;
        let shares = self.stages.len();
        let mut eps = Vec::with_capacity(shares);
        for _ in 0..shares {
            eps.push(beta_quantile(self.dim, beta / shares as f64)?);
        }
        Ok(eps)
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    pub fn index_id(&self) -> &str {
        &self.index_id
    }

    pub fn stages(&self) -> &[usize] {
        &self.stages
    }

    pub fn vectors(&self) -> &[f32] {
        &self.vectors
    }

    pub fn save(&self, dir: &Path) -> Result<(), IndexError> {
        fs::create_dir_all(dir)?;
        let manifest = Manifest {
            schema: crate::certificate::SCHEMA.to_string(),
            dim: self.dim,
            n: self.n,
            partitions: self.partitions,
            stages: self.stages.clone(),
            seed_commitment: self.seed_commitment.clone(),
            index_id: self.index_id.clone(),
        };
        fs::write(dir.join(MANIFEST), serde_json::to_vec_pretty(&manifest)?)?;
        fs::write(dir.join(VECTORS), f32_bytes(&self.vectors))?;
        fs::write(dir.join(CENTROIDS), f32_bytes(&self.centroids))?;
        fs::write(dir.join(ASSIGN), u32_bytes(&self.assign))?;
        fs::write(dir.join(NORMS), f32_bytes(&self.norms))?;
        fs::write(dir.join(CODES), u64_bytes(&self.codes))?;
        let mut coeffs = Vec::with_capacity(self.coeff_a.len() * 2);
        for i in 0..self.coeff_a.len() {
            coeffs.push(self.coeff_a[i]);
            coeffs.push(self.coeff_k[i]);
        }
        fs::write(dir.join(COEFFS), f64_bytes(&coeffs))?;
        fs::write(dir.join(COEFF_C), f64_bytes(&self.coeff_c))?;
        fs::write(dir.join(ORDER), u32_bytes(&self.order))?;
        fs::write(dir.join(STARTS), u32_bytes(&self.starts))?;
        Ok(())
    }

    pub fn load(dir: &Path, seed: &[u8; 32]) -> Result<Self, IndexError> {
        let manifest: Manifest = serde_json::from_slice(&fs::read(dir.join(MANIFEST))?)?;
        if manifest.schema != crate::certificate::SCHEMA {
            return Err(IndexError::Corrupt("schema".to_string()));
        }
        let commitment = hex(&blake3::hash(seed));
        if commitment != manifest.seed_commitment {
            return Err(IndexError::SeedMismatch);
        }
        let dim = manifest.dim;
        let n = manifest.n;
        let words = dim.div_ceil(64);
        let vectors = read_f32(&fs::read(dir.join(VECTORS))?, n * dim)?;
        let centroids = read_f32(&fs::read(dir.join(CENTROIDS))?, manifest.partitions * dim)?;
        let assign = read_u32(&fs::read(dir.join(ASSIGN))?, n)?;
        let norms = read_f32(&fs::read(dir.join(NORMS))?, n)?;
        if norms.iter().any(|v| !v.is_finite()) {
            return Err(IndexError::NonFinite);
        }
        let codes = read_u64(&fs::read(dir.join(CODES))?, n * words)?;
        let coeffs = read_f64(&fs::read(dir.join(COEFFS))?, n * manifest.stages.len() * 2)?;
        let mut coeff_a = Vec::with_capacity(n * manifest.stages.len());
        let mut coeff_k = Vec::with_capacity(n * manifest.stages.len());
        for chunk in coeffs.chunks_exact(2) {
            coeff_a.push(chunk[0]);
            coeff_k.push(chunk[1]);
        }
        let coeff_c = read_f64(&fs::read(dir.join(COEFF_C))?, n * manifest.stages.len())?;
        let order = read_u32(&fs::read(dir.join(ORDER))?, n)?;
        let starts = read_u32(&fs::read(dir.join(STARTS))?, manifest.partitions + 1)?;
        let mut index = Self {
            dim,
            n,
            partitions: manifest.partitions,
            stages: manifest.stages,
            words,
            vectors,
            centroids,
            assign,
            order,
            starts,
            norms,
            codes,
            coeff_a,
            coeff_k,
            coeff_c,
            scan_norms: Vec::new(),
            scan_codes: Vec::new(),
            scan_a: Vec::new(),
            scan_k: Vec::new(),
            scan_c: Vec::new(),
            scan_ids: Vec::new(),
            rotation: Rotation::from_seed(dim, seed),
            seed_commitment: manifest.seed_commitment,
            index_id: manifest.index_id.clone(),
        };
        if index.compute_id() != manifest.index_id {
            return Err(IndexError::Checksum);
        }
        index.build_scan_layout();
        index.index_id = manifest.index_id;
        Ok(index)
    }

    fn compute_id(&self) -> String {
        let mut h = blake3::Hasher::new();
        h.update(crate::certificate::SCHEMA.as_bytes());
        h.update(&(self.dim as u64).to_le_bytes());
        h.update(&(self.n as u64).to_le_bytes());
        h.update(&(self.partitions as u64).to_le_bytes());
        for m in &self.stages {
            h.update(&(*m as u64).to_le_bytes());
        }
        h.update(self.seed_commitment.as_bytes());
        h.update(f32_bytes(&self.vectors).as_slice());
        h.update(f32_bytes(&self.centroids).as_slice());
        h.update(u32_bytes(&self.assign).as_slice());
        h.update(u32_bytes(&self.order).as_slice());
        h.update(u32_bytes(&self.starts).as_slice());
        h.update(f32_bytes(&self.norms).as_slice());
        h.update(u64_bytes(&self.codes).as_slice());
        h.update(f64_bytes(&self.coeff_a).as_slice());
        h.update(f64_bytes(&self.coeff_k).as_slice());
        h.update(f64_bytes(&self.coeff_c).as_slice());
        format!("blake3:{}", h.finalize().to_hex())
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    schema: String,
    dim: usize,
    n: usize,
    partitions: usize,
    stages: Vec<usize>,
    seed_commitment: String,
    index_id: String,
}

fn default_stages(dim: usize) -> Vec<usize> {
    let mut stages: Vec<usize> = [128usize, 256].into_iter().filter(|m| *m < dim).collect();
    stages.push(dim);
    stages
}

fn kmeans(
    vectors: &[f32],
    n: usize,
    dim: usize,
    partitions: usize,
    iterations: usize,
    rng: &mut ChaCha20Rng,
) -> (Vec<f32>, Vec<u32>) {
    let mut centroids = Vec::with_capacity(partitions * dim);
    let first = rng.random_range(0..n);
    centroids.extend_from_slice(&vectors[first * dim..(first + 1) * dim]);
    let mut closest = vec![f64::INFINITY; n];
    for _ in 1..partitions {
        let last = centroids.len() / dim - 1;
        let mut total = 0.0;
        for (id, slot) in closest.iter_mut().enumerate() {
            let d = distance2(vectors, id, &centroids, last, dim);
            if d < *slot {
                *slot = d;
            }
            total += *slot;
        }
        let mut pick = rng.random::<f64>() * total;
        let mut chosen = n - 1;
        for (id, slot) in closest.iter().enumerate() {
            pick -= *slot;
            if pick <= 0.0 {
                chosen = id;
                break;
            }
        }
        centroids.extend_from_slice(&vectors[chosen * dim..(chosen + 1) * dim]);
    }

    let mut assign = vec![0u32; n];
    for _ in 0..iterations {
        let mut changed = false;
        for (id, slot) in assign.iter_mut().enumerate() {
            let mut best = 0usize;
            let mut best_d = f64::INFINITY;
            for p in 0..partitions {
                let d = distance2(vectors, id, &centroids, p, dim);
                if d < best_d {
                    best_d = d;
                    best = p;
                }
            }
            if *slot as usize != best {
                *slot = best as u32;
                changed = true;
            }
        }
        let mut sums = vec![0f64; partitions * dim];
        let mut counts = vec![0usize; partitions];
        for (id, slot) in assign.iter().enumerate() {
            let p = *slot as usize;
            counts[p] += 1;
            for j in 0..dim {
                sums[p * dim + j] += vectors[id * dim + j] as f64;
            }
        }
        for p in 0..partitions {
            if counts[p] == 0 {
                continue;
            }
            let inv = 1.0 / counts[p] as f64;
            for j in 0..dim {
                centroids[p * dim + j] = (sums[p * dim + j] * inv) as f32;
            }
        }
        if !changed {
            break;
        }
    }
    (centroids, assign)
}

fn partition_order(assign: &[u32], partitions: usize) -> (Vec<u32>, Vec<u32>) {
    let mut starts = vec![0u32; partitions + 1];
    for a in assign {
        starts[*a as usize + 1] += 1;
    }
    for p in 1..=partitions {
        starts[p] += starts[p - 1];
    }
    let mut cursor = starts.clone();
    let mut order = vec![0u32; assign.len()];
    for (id, a) in assign.iter().enumerate() {
        let slot = cursor[*a as usize] as usize;
        order[slot] = id as u32;
        cursor[*a as usize] += 1;
    }
    (order, starts)
}

fn distance2(vectors: &[f32], id: usize, centroids: &[f32], c: usize, dim: usize) -> f64 {
    let mut s = 0.0;
    for j in 0..dim {
        let d = vectors[id * dim + j] as f64 - centroids[c * dim + j] as f64;
        s += d * d;
    }
    s
}

fn next_up(x: f64) -> f64 {
    if x.is_nan() || x == f64::INFINITY {
        return x;
    }
    if x == 0.0 {
        return f64::from_bits(1);
    }
    if x > 0.0 {
        f64::from_bits(x.to_bits() + 1)
    } else {
        f64::from_bits(x.to_bits() - 1)
    }
}

fn next_down(x: f64) -> f64 {
    if x.is_nan() || x == f64::NEG_INFINITY {
        return x;
    }
    if x == 0.0 {
        return f64::from_bits(0x8000_0000_0000_0001);
    }
    if x > 0.0 {
        f64::from_bits(x.to_bits() - 1)
    } else {
        f64::from_bits(x.to_bits() + 1)
    }
}

fn exact_distance2(vectors: &[f32], id: usize, dim: usize, q: &[f64]) -> f64 {
    let mut s = 0.0;
    for j in 0..dim {
        let d = vectors[id * dim + j] as f64 - q[j];
        s += d * d;
    }
    s
}

fn push_candidate(candidates: &mut Vec<(f64, u32)>, d: f64, id: u32, k: usize) {
    if candidates.iter().any(|(_, existing)| *existing == id) {
        return;
    }
    candidates.push((d, id));
    candidates.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
    if candidates.len() > k {
        candidates.pop();
    }
}

fn hex(hash: &blake3::Hash) -> String {
    format!("blake3:{}", hash.to_hex())
}

fn f32_bytes(values: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 4);
    for v in values {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

fn f64_bytes(values: &[f64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 8);
    for v in values {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

fn u32_bytes(values: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 4);
    for v in values {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

fn u64_bytes(values: &[u64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 8);
    for v in values {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

fn read_f32(bytes: &[u8], expected: usize) -> Result<Vec<f32>, IndexError> {
    if bytes.len() != expected * 4 {
        return Err(IndexError::Corrupt("f32 length".to_string()));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect())
}

fn read_f64(bytes: &[u8], expected: usize) -> Result<Vec<f64>, IndexError> {
    if bytes.len() != expected * 8 {
        return Err(IndexError::Corrupt("f64 length".to_string()));
    }
    Ok(bytes
        .chunks_exact(8)
        .map(|b| f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
        .collect())
}

fn read_u32(bytes: &[u8], expected: usize) -> Result<Vec<u32>, IndexError> {
    if bytes.len() != expected * 4 {
        return Err(IndexError::Corrupt("u32 length".to_string()));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect())
}

fn read_u64(bytes: &[u8], expected: usize) -> Result<Vec<u64>, IndexError> {
    if bytes.len() != expected * 8 {
        return Err(IndexError::Corrupt("u64 length".to_string()));
    }
    Ok(bytes
        .chunks_exact(8)
        .map(|b| u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_up_down_step_one_ulp() {
        assert!(next_up(1.0) > 1.0);
        assert!(next_down(1.0) < 1.0);
        assert!(next_up(-1.0) > -1.0);
        assert!(next_down(-1.0) < -1.0);
        assert_eq!(next_up(f64::INFINITY), f64::INFINITY);
        assert_eq!(next_down(f64::NEG_INFINITY), f64::NEG_INFINITY);
    }
    #[test]
    fn fast_stage_dot_matches_naive_channels() {
        let q: Vec<f64> = (0..128).map(|i| (i as f64 * 0.37).sin()).collect();
        for m in [4usize, 9, 16, 96, 128] {
            let stage = FastStage::new(&q, m);
            let words = m.div_ceil(64);
            let codes: Vec<u64> = (0..words)
                .map(|w| 0x9e37_79b9_7f4a_7c15u64.wrapping_mul(w as u64 + 1))
                .collect();
            let mut naive = 0.0;
            for j in 0..m {
                let bit = (codes[j / 64] >> (j % 64)) & 1;
                naive += if bit == 1 { q[j] } else { -q[j] };
            }
            assert!((stage.dot(&codes, 0) - naive).abs() < 1e-9);
        }
    }
}
