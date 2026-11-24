use std::fs;
use std::io::{BufRead, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use clap::{Parser, Subcommand};
use pqr_core::audit::{audit, AuditConfig};
use pqr_core::certificate::{Certificate, DegradedReason, Target};
use pqr_core::index::{distance_then_id, BuildConfig, PqrIndex, QueryOptions, QueryOutcome};
use pqr_core::seed::Seed;

type CliResult<T> = Result<T, Box<dyn std::error::Error>>;

fn config_error(message: impl Into<String>) -> Box<dyn std::error::Error> {
    Box::new(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        message.into(),
    ))
}

#[derive(Parser)]
#[command(
    name = "pqr-guard",
    version,
    about = "Per-query recall certificates for ANN search"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Build(BuildArgs),
    Query(QueryArgs),
    Audit(AuditArgs),
    Bench(BenchArgs),
    Verify(VerifyArgs),
}

#[derive(clap::Args)]
struct VerifyArgs {
    #[arg(long)]
    certificates: PathBuf,
}

#[derive(clap::Args)]
struct BuildArgs {
    #[arg(long)]
    vectors: PathBuf,
    #[arg(long)]
    dim: Option<usize>,
    #[arg(long, default_value = "l2")]
    metric: String,
    #[arg(long, default_value_t = 0)]
    partitions: usize,
    #[arg(long, default_value = "auto")]
    code_bits: String,
    #[command(flatten)]
    seed: SeedArgs,
    #[arg(long)]
    out: PathBuf,
    #[arg(long, default_value_t = 10)]
    iterations: usize,
}

#[derive(clap::Args)]
struct QueryArgs {
    #[arg(long)]
    index: PathBuf,
    #[arg(long)]
    queries: PathBuf,
    #[command(flatten)]
    seed: SeedArgs,
    #[arg(long)]
    k: usize,
    #[arg(long)]
    tau: f64,
    #[arg(long)]
    delta: f64,
    #[arg(long, default_value_t = 4)]
    nprobe: usize,
    #[arg(long, default_value = "certs.jsonl")]
    certificates: PathBuf,
    #[arg(long)]
    results: Option<PathBuf>,
    #[arg(long)]
    adaptive: bool,
    #[arg(long)]
    public_seed: bool,
    #[arg(long)]
    filtered: bool,
    #[arg(long)]
    approximate_rerank: bool,
}

#[derive(clap::Args)]
struct AuditArgs {
    #[arg(long)]
    index: PathBuf,
    #[arg(long)]
    queries: PathBuf,
    #[command(flatten)]
    seed: SeedArgs,
    #[arg(long)]
    k: usize,
    #[arg(long)]
    tau: f64,
    #[arg(long)]
    delta: f64,
    #[arg(long, default_value_t = 64)]
    rotations: u64,
    #[arg(long, default_value_t = 4)]
    nprobe: usize,
    #[arg(long)]
    gt: Option<PathBuf>,
    #[arg(long)]
    report: PathBuf,
    #[arg(long)]
    audit_seed_file: Option<PathBuf>,
}

#[derive(clap::Args)]
struct BenchArgs {
    #[arg(long)]
    index: PathBuf,
    #[arg(long)]
    queries: PathBuf,
    #[command(flatten)]
    seed: SeedArgs,
    #[arg(long)]
    k: usize,
    #[arg(long)]
    tau: f64,
    #[arg(long)]
    delta: f64,
    #[arg(long, default_value_t = 4)]
    nprobe: usize,
    #[arg(long, default_value_t = 5)]
    repeat: usize,
    #[arg(long, default_value = "brute")]
    baseline: String,
    #[arg(long)]
    report: Option<PathBuf>,
    #[arg(long)]
    hnsw_cache: Option<PathBuf>,
}

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(4);
        }
    }
}

fn run() -> CliResult<i32> {
    match Cli::parse().command {
        Command::Build(args) => build(args),
        Command::Query(args) => query(args),
        Command::Audit(args) => run_audit(args),
        Command::Bench(args) => bench(args),
        Command::Verify(args) => verify(args),
    }
}

fn build(args: BuildArgs) -> CliResult<i32> {
    if !args.metric.eq_ignore_ascii_case("l2") {
        return Err(config_error("only --metric l2 is supported in v0.1"));
    }
    let (vectors, shape) = read_npy_f32(&args.vectors)?;
    if shape.len() != 2 {
        return Err(config_error("--vectors must be a 2-D matrix"));
    }
    let dim = match args.dim {
        Some(d) => {
            if d == 0 || shape.last().copied() != Some(d) {
                return Err(config_error("--dim does not match the vector matrix"));
            }
            d
        }
        None => *shape
            .last()
            .ok_or_else(|| config_error("empty array shape"))?,
    };
    let stages = parse_stages(&args.code_bits, dim)?;
    let seed = args.seed.read()?;
    let cfg = BuildConfig {
        partitions: args.partitions,
        stages,
        iterations: args.iterations,
    };
    let (index, receipt) = PqrIndex::build(&vectors, dim, &cfg, seed.as_bytes())?;
    drop(seed);
    index.save(&args.out)?;
    println!("{}", serde_json::to_string_pretty(&receipt)?);
    Ok(0)
}

fn query(args: QueryArgs) -> CliResult<i32> {
    let seed = args.seed.read()?;
    let index = PqrIndex::load(&args.index, seed.as_bytes())?;
    drop(seed);
    let (queries, shape) = read_npy_f32(&args.queries)?;
    let dim = index.dim();
    let nq = validate_queries(&queries, &shape, dim)?;
    let target = Target::new(args.k, args.tau, args.delta)?;
    let stage_eps = index.stage_eps(&target)?;
    let mut reasons = Vec::new();
    if args.adaptive {
        reasons.push(DegradedReason::AdaptiveSequence);
    }
    if args.public_seed {
        reasons.push(DegradedReason::PublicSeed);
    }
    if args.filtered {
        reasons.push(DegradedReason::FilteredSearch);
    }
    if args.approximate_rerank {
        reasons.push(DegradedReason::ApproximateRerank);
    }
    let opts = QueryOptions {
        target,
        nprobe: args.nprobe,
        reasons,
        stage_eps: Some(stage_eps),
    };
    let mut cert_out = BufWriter::new(fs::File::create(&args.certificates)?);
    let mut result_out = match &args.results {
        Some(path) => Some(BufWriter::new(fs::File::create(path)?)),
        None => None,
    };
    let mut degraded = false;
    for qi in 0..nq {
        let q = &queries[qi * dim..(qi + 1) * dim];
        let outcome = index.query(q, &opts)?;
        let line = match outcome {
            QueryOutcome::Certified { hits, certificate } => {
                serde_json::json!({"query": qi, "status": "certified", "certificate": certificate, "hits": hits})
            }
            QueryOutcome::Degraded { hits, degraded: d } => {
                degraded = true;
                serde_json::json!({"query": qi, "status": "degraded", "degraded": d, "hits": hits})
            }
        };
        writeln!(cert_out, "{}", serde_json::to_string(&line)?)?;
        if let Some(out) = result_out.as_mut() {
            let hits = line["hits"].clone();
            writeln!(out, "{}", serde_json::json!({"query": qi, "hits": hits}))?;
        }
    }
    Ok(if degraded { 2 } else { 0 })
}

fn run_audit(args: AuditArgs) -> CliResult<i32> {
    let seed = args.seed.read()?;
    let master_seed = match &args.audit_seed_file {
        Some(path) => Seed::from_file(path)?,
        None => {
            let mut h = blake3::Hasher::new();
            h.update(b"pqr.guard.audit.master");
            h.update(seed.as_bytes());
            Seed::from_bytes(*h.finalize().as_bytes())
        }
    };
    if args.rotations == 0 {
        return Err(config_error("--rotations must be positive"));
    }
    let mut index = PqrIndex::load(&args.index, seed.as_bytes())?;
    drop(seed);
    let (queries, shape) = read_npy_f32(&args.queries)?;
    validate_queries(&queries, &shape, index.dim())?;
    let target = Target::new(args.k, args.tau, args.delta)?;
    let gt = match &args.gt {
        Some(path) => Some(read_npy_i64(path)?),
        None => None,
    };
    let cfg = AuditConfig {
        rotations: args.rotations,
        nprobe: args.nprobe,
        master_seed,
    };
    let report = audit(&mut index, &queries, target, &cfg, gt.as_deref())?;
    fs::write(&args.report, serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(if report.verdict == "pass" { 0 } else { 3 })
}

#[derive(serde::Serialize)]
struct VerifySummary {
    lines: usize,
    valid: usize,
    invalid: usize,
    errors: Vec<String>,
}

fn verify(args: VerifyArgs) -> CliResult<i32> {
    let file = fs::File::open(&args.certificates)?;
    let mut summary = VerifySummary {
        lines: 0,
        valid: 0,
        invalid: 0,
        errors: Vec::new(),
    };
    for line in std::io::BufReader::new(file).lines() {
        let line = line?;
        summary.lines += 1;
        match verify_line(&line) {
            Ok(()) => summary.valid += 1,
            Err(error) => {
                summary.invalid += 1;
                if summary.errors.len() < 20 {
                    summary
                        .errors
                        .push(format!("line {}: {error}", summary.lines));
                }
            }
        }
    }
    let code = if summary.invalid > 0 { 2 } else { 0 };
    println!("{}", serde_json::to_string(&summary)?);
    Ok(code)
}

fn verify_line(line: &str) -> CliResult<()> {
    let value: serde_json::Value = serde_json::from_str(line)?;
    let certificate = match value.get("status").and_then(|s| s.as_str()) {
        Some("degraded") => return Err(config_error("degraded record cannot verify as certified")),
        Some("certified") => value
            .get("certificate")
            .ok_or_else(|| config_error("certified record has no certificate"))?,
        _ => &value,
    };
    let certificate: Certificate = serde_json::from_value(certificate.clone())?;
    certificate.verify()?;
    Ok(())
}

#[derive(serde::Serialize)]
struct BenchReport {
    queries: usize,
    k: usize,
    repeat: usize,
    pqr_p50_ms: f64,
    pqr_p95_ms: f64,
    brute_p50_ms: Option<f64>,
    speedup_over_brute: Option<f64>,
    hnsw_p50_ms: Option<f64>,
    hnsw_p95_ms: Option<f64>,
    hnsw_ef_search: Option<usize>,
    hnsw_recall: Option<f64>,
    pqr_over_hnsw: Option<f64>,
    baseline: String,
}

fn bench(args: BenchArgs) -> CliResult<i32> {
    if args.baseline != "brute" && args.baseline != "usearch-hnsw" && args.baseline != "none" {
        return Err(config_error(
            "baseline must be brute, usearch-hnsw, or none",
        ));
    }
    let seed = args.seed.read()?;
    let index = PqrIndex::load(&args.index, seed.as_bytes())?;
    drop(seed);
    let (queries, shape) = read_npy_f32(&args.queries)?;
    let dim = index.dim();
    let nq = validate_queries(&queries, &shape, dim)?;
    if args.repeat == 0 {
        return Err(config_error("--repeat must be positive"));
    }
    let target = Target::new(args.k, args.tau, args.delta)?;
    let stage_eps = index.stage_eps(&target)?;
    let opts = QueryOptions {
        target,
        nprobe: args.nprobe,
        reasons: Vec::new(),
        stage_eps: Some(stage_eps),
    };
    let mut pqr = Vec::new();
    let mut brute = Vec::new();
    for _ in 0..args.repeat {
        for qi in 0..nq {
            let q = &queries[qi * dim..(qi + 1) * dim];
            let t = Instant::now();
            let _ = index.query(q, &opts)?;
            pqr.push(t.elapsed().as_secs_f64() * 1000.0);
            if args.baseline == "brute" {
                let t = Instant::now();
                let _ = brute_top(&index, q, args.k);
                brute.push(t.elapsed().as_secs_f64() * 1000.0);
            }
        }
    }
    pqr.sort_by(f64::total_cmp);
    brute.sort_by(f64::total_cmp);
    let p50 = pqr[pqr.len() / 2];
    let p95 = pqr[(pqr.len() * 95 / 100).min(pqr.len() - 1)];
    let b50 = (!brute.is_empty()).then(|| brute[brute.len() / 2]);
    let mut hnsw_p50 = None;
    let mut hnsw_p95 = None;
    let mut hnsw_ef = None;
    let mut hnsw_recall = None;
    let mut ratio = None;
    if args.baseline == "usearch-hnsw" {
        let hnsw = build_hnsw(&index, args.hnsw_cache.as_deref())?;
        let truth: Vec<Vec<u64>> = (0..nq)
            .map(|qi| brute_top(&index, &queries[qi * dim..(qi + 1) * dim], args.k))
            .collect();
        let mut chosen = 512usize;
        for ef in [16usize, 32, 64, 128, 256, 512] {
            hnsw.change_expansion_search(ef);
            let mut found = 0usize;
            for qi in 0..nq {
                let q = &queries[qi * dim..(qi + 1) * dim];
                let got = hnsw.search(q, args.k)?;
                found += got
                    .keys
                    .iter()
                    .filter(|k| truth[qi].contains(&(**k)))
                    .count();
            }
            let recall = found as f64 / (nq * args.k) as f64;
            chosen = ef;
            if recall >= 0.99 {
                break;
            }
        }
        hnsw.change_expansion_search(chosen);
        let mut times = Vec::new();
        let mut found = 0usize;
        for _ in 0..args.repeat {
            for qi in 0..nq {
                let q = &queries[qi * dim..(qi + 1) * dim];
                let t = Instant::now();
                let got = hnsw.search(q, args.k)?;
                times.push(t.elapsed().as_secs_f64() * 1000.0);
                found += got
                    .keys
                    .iter()
                    .filter(|k| truth[qi].contains(&(**k)))
                    .count();
            }
        }
        times.sort_by(f64::total_cmp);
        let h50 = times[times.len() / 2];
        hnsw_p50 = Some(h50);
        hnsw_p95 = Some(times[(times.len() * 95 / 100).min(times.len() - 1)]);
        hnsw_ef = Some(chosen);
        let recall = found as f64 / (nq * args.k * args.repeat) as f64;
        hnsw_recall = Some(recall);
        ratio = Some(p50 / h50);
    }
    let report = BenchReport {
        queries: nq,
        k: args.k,
        repeat: args.repeat,
        pqr_p50_ms: p50,
        pqr_p95_ms: p95,
        brute_p50_ms: b50,
        speedup_over_brute: b50.map(|b| b / p50),
        hnsw_p50_ms: hnsw_p50,
        hnsw_p95_ms: hnsw_p95,
        hnsw_ef_search: hnsw_ef,
        hnsw_recall,
        pqr_over_hnsw: ratio,
        baseline: args.baseline,
    };
    let json = serde_json::to_vec_pretty(&report)?;
    if let Some(path) = &args.report {
        fs::write(path, &json)?;
    }
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(0)
}

fn build_hnsw(index: &PqrIndex, cache: Option<&Path>) -> CliResult<usearch::Index> {
    use usearch::{Index, IndexOptions, MetricKind, ScalarKind};
    let options = IndexOptions {
        dimensions: index.dim(),
        metric: MetricKind::L2sq,
        quantization: ScalarKind::F32,
        connectivity: 32,
        expansion_add: 200,
        expansion_search: 0,
        multi: false,
    };
    let hnsw = Index::new(&options)?;
    if let Some(path) = cache {
        if path.exists() {
            if let Some(text) = path.to_str() {
                if hnsw.load(text).is_ok() {
                    return Ok(hnsw);
                }
            }
        }
    }
    hnsw.reserve(index.len())?;
    let dim = index.dim();
    let vectors = index.vectors();
    for id in 0..index.len() {
        hnsw.add(id as u64, &vectors[id * dim..(id + 1) * dim])?;
    }
    if let Some(path) = cache {
        if let Some(text) = path.to_str() {
            hnsw.save(text)?;
        }
    }
    Ok(hnsw)
}

fn brute_top(index: &PqrIndex, q: &[f32], k: usize) -> Vec<u64> {
    let dim = index.dim();
    let vectors = index.vectors();
    let mut all: Vec<(f64, u64)> = (0..vectors.len() / dim)
        .map(|id| {
            let mut s = 0.0;
            for j in 0..dim {
                let d = vectors[id * dim + j] as f64 - q[j] as f64;
                s += d * d;
            }
            (s, id as u64)
        })
        .collect();
    all.sort_unstable_by(distance_then_id);
    all.into_iter().take(k).map(|(_, id)| id).collect()
}

fn validate_queries(queries: &[f32], shape: &[usize], dim: usize) -> CliResult<usize> {
    if shape.len() != 2 || shape[1] != dim || queries.is_empty() || queries.len() % dim != 0 {
        return Err(config_error(
            "query matrix must be nonempty and match index dim",
        ));
    }
    Ok(queries.len() / dim)
}

#[derive(clap::Args)]
struct SeedArgs {
    #[arg(long, group = "seed", value_name = "PATH")]
    seed_file: Option<PathBuf>,
    #[arg(long, group = "seed", value_name = "VAR")]
    seed_env: Option<String>,
    #[arg(long, group = "seed", value_name = "CMD")]
    seed_command: Option<String>,
}

impl SeedArgs {
    fn read(&self) -> CliResult<Seed> {
        if let Some(path) = &self.seed_file {
            return Ok(Seed::from_file(path)?);
        }
        if let Some(name) = &self.seed_env {
            return Ok(Seed::from_env(name)?);
        }
        if let Some(command) = &self.seed_command {
            return Ok(Seed::from_command(command)?);
        }
        Err(config_error(
            "one of --seed-file, --seed-env, or --seed-command is required",
        ))
    }
}

fn parse_stages(spec: &str, dim: usize) -> CliResult<Vec<usize>> {
    if spec == "auto" {
        let mut stages: Vec<usize> = [128usize, 256].into_iter().filter(|m| *m < dim).collect();
        stages.push(dim);
        return Ok(stages);
    }
    let values: Result<Vec<usize>, _> = spec
        .split(',')
        .map(|part| part.trim().parse::<usize>())
        .collect();
    let mut stages = values.map_err(|_| config_error("invalid --code-bits"))?;
    if stages.len() == 1 && stages[0] < dim {
        stages.push(dim);
    }
    if stages.is_empty()
        || stages.iter().any(|m| *m == 0 || *m > dim)
        || stages.windows(2).any(|w| w[0] >= w[1])
    {
        return Err(config_error(
            "--code-bits must be increasing and within dim",
        ));
    }
    Ok(stages)
}

fn checked_npy(bytes: &[u8], item_size: u64) -> CliResult<npyz::NpyFile<std::io::Cursor<&[u8]>>> {
    let probe = npyz::NpyFile::new(std::io::Cursor::new(bytes))?;
    let header = probe.header().clone();
    let cursor = probe.into_inner();
    let offset = cursor.position() as usize;
    let claimed = header
        .len()
        .checked_mul(item_size)
        .ok_or_else(|| config_error("npy shape exceeds file size"))?;
    let available = bytes.len().saturating_sub(offset) as u64;
    if claimed > available {
        return Err(config_error("npy shape exceeds file size"));
    }
    Ok(npyz::NpyFile::with_header(header, cursor))
}

fn read_npy_f32(path: &Path) -> CliResult<(Vec<f32>, Vec<usize>)> {
    let bytes = fs::read(path)?;
    let npy = checked_npy(&bytes, 4)?;
    let shape: Vec<usize> = npy.shape().iter().map(|v| *v as usize).collect();
    let data = npy.into_vec::<f32>()?;
    Ok((data, shape))
}

fn read_npy_i64(path: &Path) -> CliResult<Vec<u64>> {
    let bytes = fs::read(path)?;
    let npy = checked_npy(&bytes, 8)?;
    let data = npy.into_vec::<i64>()?;
    data.into_iter().map(gt_id).collect()
}

fn gt_id(value: i64) -> CliResult<u64> {
    u64::try_from(value).map_err(|_| config_error("ground truth ids must be nonnegative"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_decoder_accepts_raw_and_hex() {
        let raw = [7u8; 32];
        assert_eq!(Seed::parse(&raw, false).unwrap().as_bytes(), &raw);
        let hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(Seed::parse(hex.as_bytes(), true).unwrap().as_bytes(), &raw);
        assert!(Seed::parse(b"short", true).is_err());
        assert!(Seed::parse(&[b'z'; 64], true).is_err());
    }

    #[test]
    fn verify_rejects_tampered_and_degraded_lines() {
        let target = Target::new(10, 0.9, 0.01).unwrap();
        let cert = Certificate::certified("blake3:abc", 64, &target, &[16, 32, 64]);
        let valid = serde_json::to_string(&serde_json::json!({
            "query": 0,
            "status": "certified",
            "certificate": cert.clone(),
            "hits": []
        }))
        .unwrap();
        let mut bad = cert.clone();
        bad.claim.beta += 0.1;
        let tampered = serde_json::to_string(&serde_json::json!({
            "query": 1,
            "status": "certified",
            "certificate": bad,
            "hits": []
        }))
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("certs.jsonl");
        fs::write(&path, format!("{valid}\n{tampered}\n")).unwrap();
        assert_eq!(
            verify(VerifyArgs {
                certificates: path.clone()
            })
            .unwrap(),
            2
        );
        fs::write(&path, format!("{valid}\n")).unwrap();
        assert_eq!(verify(VerifyArgs { certificates: path }).unwrap(), 0);
    }

    #[test]
    fn ground_truth_ids_must_be_nonnegative() {
        assert_eq!(gt_id(7).unwrap(), 7);
        assert!(gt_id(-1).is_err());
    }

    #[test]
    fn npy_shape_must_fit_file() {
        use npyz::WriterBuilder;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("q.npy");
        let mut writer = npyz::WriteOptions::<f32>::new()
            .default_dtype()
            .shape(&[20, 16])
            .writer(fs::File::create(&path).unwrap())
            .begin_nd()
            .unwrap();
        writer.extend(std::iter::repeat_n(0f32, 320)).unwrap();
        writer.finish().unwrap();

        let bytes = fs::read(path).unwrap();
        assert!(checked_npy(&bytes, 4).is_ok());
        assert!(checked_npy(&bytes, 8).is_err());
    }

    #[test]
    fn query_shape_must_match_index() {
        let data = vec![0f32; 128];
        assert_eq!(validate_queries(&data, &[1, 128], 128).unwrap(), 1);
        assert!(validate_queries(&data, &[2, 64], 128).is_err());
        assert!(validate_queries(&[], &[0, 128], 128).is_err());
    }
}
