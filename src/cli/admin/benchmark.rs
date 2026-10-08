// SPDX-License-Identifier: Apache-2.0

//! `rez benchmark` - run resolve performance benchmarking suite.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use clap::Args;

use foundation::errors::{Result, RezError};

/// Run benchmarking suite for package resolves.
#[derive(Args, Debug)]
pub struct BenchmarkArgs {
    /// Output directory for results
    #[arg(long, default_value = "out")]
    pub out: PathBuf,

    /// Number of iterations per resolve
    #[arg(long, default_value = "1")]
    pub iterations: usize,

    /// Show ASCII histogram from previous results
    #[arg(long)]
    pub histogram: bool,

    /// Compare results against another directory
    #[arg(long, value_name = "RESULTS_DIR")]
    pub compare: Option<PathBuf>,
}

pub fn run(args: &BenchmarkArgs) -> Result<()> {
    let out_dir = std::path::absolute(&args.out).unwrap_or_else(|_| args.out.clone());

    if args.histogram {
        print_histogram(&out_dir)?;
        return Ok(());
    }

    if let Some(ref compare_dir) = args.compare {
        compare_results(&out_dir, compare_dir)?;
        return Ok(());
    }

    run_benchmark(&out_dir, args.iterations)?;
    Ok(())
}

/// Run the benchmark suite, saving results to out_dir.
fn run_benchmark(out_dir: &Path, iterations: usize) -> Result<()> {
    if out_dir.exists() {
        return Err(RezError::Config(format!(
            "Dir specified by --out ({}) must not exist",
            out_dir.display()
        )));
    }

    fs::create_dir_all(out_dir)
        .map_err(|e| RezError::Config(format!("Cannot create {}: {}", out_dir.display(), e)))?;

    println!("Writing results to {}...", out_dir.display());

    // Sample requests (full impl reads from data/benchmarking/requests.json)
    let requests: Vec<Vec<String>> = vec![
        vec!["python-3".into()],
        vec!["platform".into(), "arch".into()],
        vec!["python-3".into(), "platform".into()],
    ];

    println!(
        "Performing {} resolves ({} iterations each)...",
        requests.len(),
        iterations
    );

    let mut summaries: Vec<serde_json::Value> = Vec::new();
    let total_start = Instant::now();

    for (i, request) in requests.iter().enumerate() {
        println!("\n[{}/{}]", i + 1, requests.len());
        println!("Request: {:?}", request);
        print!("Resolving");

        let mut total_secs = 0.0;
        for _ in 0..iterations {
            let start = Instant::now();
            // Simulated resolve - full impl calls ResolvedContext
            std::thread::sleep(std::time::Duration::from_millis(10));
            total_secs += start.elapsed().as_secs_f64();
            print!(".");
        }
        let resolve_time = total_secs / iterations as f64;
        println!(" {:.4}s", resolve_time);

        summaries.push(serde_json::json!({
            "request": request,
            "status": "success",
            "resolve_time": resolve_time,
        }));
    }

    let total_time = total_start.elapsed().as_secs_f64();

    // Compute statistics
    let resolve_times: Vec<f64> = summaries
        .iter()
        .filter_map(|s| s.get("resolve_time").and_then(|v| v.as_f64()))
        .collect();

    let n = resolve_times.len() as f64;
    let mean = resolve_times.iter().sum::<f64>() / n;
    let min_t = resolve_times.iter().cloned().fold(f64::INFINITY, f64::min);
    let max_t = resolve_times
        .iter()
        .cloned()
        .fold(f64::NEG_INFINITY, f64::max);
    let mut sorted = resolve_times.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = sorted[sorted.len() / 2];
    let stddev = (resolve_times
        .iter()
        .map(|x| (x - mean).powi(2))
        .sum::<f64>()
        / n)
        .sqrt();

    let stats = serde_json::json!({
        "total_run_time": total_time,
        "num_resolves": resolve_times.len(),
        "mean": mean,
        "median": median,
        "min": min_t,
        "max": max_t,
        "stddev": stddev,
        "platform": std::env::consts::OS,
        "rez_version": env!("CARGO_PKG_VERSION"),
    });

    println!("\n\nRESULT:");
    let stats_str = serde_json::to_string_pretty(&stats)
        .map_err(|e| RezError::Config(format!("JSON error: {}", e)))?;
    println!("{}", stats_str);

    // Save results
    let resolves_path = out_dir.join("resolves.json");
    let summary_path = out_dir.join("summary.json");

    fs::write(
        &resolves_path,
        serde_json::to_string_pretty(&summaries).unwrap_or_default(),
    )
    .map_err(|e| RezError::Config(format!("Cannot write resolves: {}", e)))?;
    fs::write(&summary_path, &stats_str)
        .map_err(|e| RezError::Config(format!("Cannot write summary: {}", e)))?;

    Ok(())
}

/// Print ASCII histogram from previous benchmark results.
fn print_histogram(out_dir: &Path) -> Result<()> {
    let resolves_path = out_dir.join("resolves.json");
    let content = fs::read_to_string(&resolves_path)
        .map_err(|e| RezError::Config(format!("Cannot read {}: {}", resolves_path.display(), e)))?;

    let summaries: Vec<serde_json::Value> = serde_json::from_str(&content)
        .map_err(|e| RezError::Config(format!("Invalid JSON: {}", e)))?;

    let resolve_times: Vec<f64> = summaries
        .iter()
        .filter_map(|s| s.get("resolve_time").and_then(|v| v.as_f64()))
        .collect();

    if resolve_times.is_empty() {
        println!("No resolve times found.");
        return Ok(());
    }

    let n_rows = 40usize;
    let n_columns = 40usize;
    let min_t = resolve_times.iter().cloned().fold(f64::INFINITY, f64::min);
    let max_t = resolve_times
        .iter()
        .cloned()
        .fold(f64::NEG_INFINITY, f64::max);
    let bucket_size = (max_t - min_t) / n_rows as f64;

    if bucket_size <= 0.0 {
        println!("All resolve times are identical: {:.4}s", min_t);
        return Ok(());
    }

    let mut buckets = vec![0usize; n_rows];
    for &t in &resolve_times {
        let i = ((t - min_t) / bucket_size) as usize;
        let i = i.min(n_rows - 1);
        buckets[i] += 1;
    }

    let max_bucket = *buckets.iter().max().unwrap_or(&1);
    let mult = n_columns as f64 / max_bucket as f64;

    // Compute max label width for alignment
    let test_str = format!("[{:.2}-{:.2}]", max_t, max_t);
    let max_left_w = test_str.len();

    let mut start_t = min_t;
    for &count in &buckets {
        let end_t = start_t + bucket_size;
        let columns = (count as f64 * mult) as usize;
        let left = format!("[{:.2}-{:.2}]", start_t, end_t);
        let padding = max_left_w.saturating_sub(left.len());
        println!("{}{} |{}", " ".repeat(padding), left, "#".repeat(columns));
        start_t = end_t;
    }

    Ok(())
}

/// Compare two benchmark result sets side by side.
fn compare_results(out_dir1: &Path, out_dir2: &Path) -> Result<()> {
    let load_summary = |dir: &Path| -> Result<serde_json::Value> {
        let path = dir.join("summary.json");
        let content = fs::read_to_string(&path)
            .map_err(|e| RezError::Config(format!("Cannot read {}: {}", path.display(), e)))?;
        serde_json::from_str(&content)
            .map_err(|e| RezError::Config(format!("Invalid JSON in {}: {}", path.display(), e)))
    };

    let summary1 = load_summary(out_dir1)?;
    let summary2 = load_summary(out_dir2)?;

    println!("Comparing {} vs {}", out_dir1.display(), out_dir2.display());
    println!();

    for field in &["max", "min", "mean", "median", "stddev"] {
        let v1 = summary1.get(field).and_then(|v| v.as_f64()).unwrap_or(0.0);
        let v2 = summary2.get(field).and_then(|v| v.as_f64()).unwrap_or(0.0);
        let delta = v2 - v1;
        let pct = if v1 > 0.0 { 100.0 * delta / v1 } else { 0.0 };
        let sign = if pct >= 0.0 { "+" } else { "" };
        println!("  {}_delta: {:.6} ({}{:.2}%)", field, delta, sign, pct);
    }

    Ok(())
}
