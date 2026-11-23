use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use clap::{Parser, Subcommand};
use pqr_core::audit::{audit, AuditConfig};
use pqr_core::certificate::{DegradedReason, Target};
use pqr_core::index::{BuildConfig, PqrIndex, QueryOptions, QueryOutcome};

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
    #[arg(long)]
    seed_file: PathBuf,
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
    #[arg(long)]
    seed_file: PathBuf,
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
    #[arg(long)]
    seed_file: PathBuf,
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
    #[arg(long)]
    seed_file: PathBuf,
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
    let seed = read_seed(&args.seed_file)?;
    let cfg = BuildConfig {
        partitions: args.partitions,
        stages,
        iterations: args.iterations,
    };
    let (index, receipt) = PqrIndex::build(&vectors, dim, &cfg, &seed)?;
    index.save(&args.out)?;
    println!("{}", serde_json::to_string_pretty(&receipt)?);
    Ok(0)
}

fn query(args: QueryArgs) -> CliResult<i32> {
    let seed = read_seed(&args.seed_file)?;
    let index = PqrIndex::load(&args.index, &seed)?;
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
    let seed = read_seed(&args.seed_file)?;
    let mut index = PqrIndex::load(&args.index, &seed)?;
    let (queries, shape) = read_npy_f32(&args.queries)?;
    validate_queries(&queries, &shape, index.dim())?;
    let target = Target::new(args.k, args.tau, args.delta)?;
    let master_seed = match &args.audit_seed_file {
        Some(path) => read_seed(path)?,
        None => *blake3::hash(&[b"pqr.guard.audit.master".as_slice(), seed.as_slice()].concat())
            .as_bytes(),
    };
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
struct BenchReport {
    queries: usize,
    k: usize,
    repeat: usize,
    pqr_p50_ms: f64,
    pqr_p95_ms: f64,
    brute_p50_ms: f64,
    speedup_over_brute: f64,
    hnsw_p50_ms: Option<f64>,
    hnsw_p95_ms: Option<f64>,
    hnsw_ef_search: Option<usize>,
    hnsw_recall: Option<f64>,
    pqr_over_hnsw: Option<f64>,
    baseline: String,
}

fn bench(args: BenchArgs) -> CliResult<i32> {
    if args.baseline != "brute" && args.baseline != "usearch-hnsw" {
        return Err(config_error("baseline must be brute or usearch-hnsw"));
    }
    let seed = read_seed(&args.seed_file)?;
    let index = PqrIndex::load(&args.index, &seed)?;
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
            let t = Instant::now();
            let _ = brute_top(&index, q, args.k);
            brute.push(t.elapsed().as_secs_f64() * 1000.0);
        }
    }
    pqr.sort_by(f64::total_cmp);
    brute.sort_by(f64::total_cmp);
    let p50 = pqr[pqr.len() / 2];
    let p95 = pqr[(pqr.len() * 95 / 100).min(pqr.len() - 1)];
    let b50 = brute[brute.len() / 2];
    let mut hnsw_p50 = None;
    let mut hnsw_p95 = None;
    let mut hnsw_ef = None;
    let mut hnsw_recall = None;
    let mut ratio = None;
    if args.baseline == "usearch-hnsw" {
        let hnsw = build_hnsw(&index)?;
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
        speedup_over_brute: b50 / p50,
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

fn build_hnsw(index: &PqrIndex) -> CliResult<usearch::Index> {
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
    hnsw.reserve(index.len())?;
    let dim = index.dim();
    let vectors = index.vectors();
    for id in 0..index.len() {
        hnsw.add(id as u64, &vectors[id * dim..(id + 1) * dim])?;
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
    all.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
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

fn read_seed(path: &Path) -> CliResult<[u8; 32]> {
    let bytes = fs::read(path)?;
    if bytes.len() != 32 {
        return Err(config_error("seed file must contain exactly 32 bytes"));
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&bytes);
    Ok(seed)
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

fn read_npy_f32(path: &Path) -> CliResult<(Vec<f32>, Vec<usize>)> {
    let bytes = fs::read(path)?;
    let npy = npyz::NpyFile::new(std::io::Cursor::new(bytes))?;
    let shape: Vec<usize> = npy.shape().iter().map(|v| *v as usize).collect();
    let data = npy.into_vec::<f32>()?;
    Ok((data, shape))
}

fn read_npy_i64(path: &Path) -> CliResult<Vec<u64>> {
    let bytes = fs::read(path)?;
    let npy = npyz::NpyFile::new(std::io::Cursor::new(bytes))?;
    let data = npy.into_vec::<i64>()?;
    Ok(data.into_iter().map(|v| v as u64).collect())
}
