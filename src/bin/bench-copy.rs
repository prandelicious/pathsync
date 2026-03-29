use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Parser, Debug)]
#[command(name = "bench-copy")]
#[command(about = "Benchmark copy strategies on real source/target storage")]
struct Cli {
    #[arg(long)]
    source: PathBuf,

    #[arg(long)]
    target_dir: PathBuf,

    #[arg(long, default_value_t = 3)]
    runs: usize,

    #[arg(long, default_value_t = 8)]
    buffer_mb: usize,

    #[arg(long, value_enum, default_value_t = Method::Both)]
    method: Method,

    #[arg(long, default_value_t = true)]
    fsync: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum Method {
    Buffered,
    Stdio,
    Both,
}

#[derive(Clone, Debug)]
struct RunResult {
    method: &'static str,
    run: usize,
    bytes: u64,
    seconds: f64,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    validate_args(&cli)?;

    let source_size = fs::metadata(&cli.source)
        .with_context(|| format!("failed to stat source file: {}", cli.source.display()))?
        .len();
    let buffer_size = cli.buffer_mb.saturating_mul(1024 * 1024);

    println!("source     : {}", cli.source.display());
    println!("target_dir : {}", cli.target_dir.display());
    println!("size       : {}", human_bytes(source_size));
    println!("runs       : {}", cli.runs);
    println!("buffer     : {}", human_bytes(buffer_size as u64));
    println!("fsync      : {}", cli.fsync);
    println!();

    let methods = selected_methods(cli.method);
    let mut all_results = Vec::new();

    for method in methods {
        println!("method: {method}");
        let mut method_results = Vec::new();

        for run in 1..=cli.runs {
            let destination = temp_destination(&cli.target_dir, method, run);
            let started = Instant::now();
            let bytes = match method {
                "buffered" => copy_buffered(&cli.source, &destination, buffer_size, cli.fsync)?,
                "stdio" => copy_stdio(&cli.source, &destination, cli.fsync)?,
                _ => unreachable!("unexpected benchmark method"),
            };
            let elapsed = started.elapsed().as_secs_f64();

            fs::remove_file(&destination).with_context(|| {
                format!(
                    "failed to remove benchmark temp file: {}",
                    destination.display()
                )
            })?;

            let result = RunResult {
                method,
                run,
                bytes,
                seconds: elapsed,
            };
            println!(
                "  run {:>2}: {:>10} in {:>7.3}s  {:>12}",
                result.run,
                human_bytes(result.bytes),
                result.seconds,
                human_rate(result.bytes, result.seconds),
            );
            method_results.push(result.clone());
            all_results.push(result);
        }

        print_summary(&method_results);
        println!();
    }

    if all_results.len() > cli.runs {
        print_comparison(&all_results, cli.runs);
    }

    Ok(())
}

fn validate_args(cli: &Cli) -> Result<()> {
    if !cli.source.is_file() {
        bail!("source file not found: {}", cli.source.display());
    }
    if !cli.target_dir.is_dir() {
        bail!("target directory not found: {}", cli.target_dir.display());
    }
    if cli.runs == 0 {
        bail!("runs must be at least 1");
    }
    if cli.buffer_mb == 0 {
        bail!("buffer_mb must be at least 1");
    }
    Ok(())
}

fn selected_methods(method: Method) -> Vec<&'static str> {
    match method {
        Method::Buffered => vec!["buffered"],
        Method::Stdio => vec!["stdio"],
        Method::Both => vec!["buffered", "stdio"],
    }
}

fn temp_destination(target_dir: &Path, method: &str, run: usize) -> PathBuf {
    target_dir.join(format!(".pathsync-bench-{method}-{run}.tmp"))
}

fn copy_buffered(
    source: &Path,
    dest: &Path,
    buffer_size: usize,
    fsync_enabled: bool,
) -> Result<u64> {
    let source_file = File::open(source)
        .with_context(|| format!("failed to open source file: {}", source.display()))?;
    let mut reader = BufReader::with_capacity(buffer_size, source_file);
    let dest_file = File::create(dest)
        .with_context(|| format!("failed to create destination file: {}", dest.display()))?;
    let mut writer = BufWriter::with_capacity(buffer_size, dest_file);
    let mut buffer = vec![0_u8; buffer_size];
    let mut copied = 0_u64;

    loop {
        let read = reader
            .read(&mut buffer)
            .with_context(|| format!("failed reading source file: {}", source.display()))?;
        if read == 0 {
            break;
        }

        writer
            .write_all(&buffer[..read])
            .with_context(|| format!("failed writing destination file: {}", dest.display()))?;
        copied += read as u64;
    }

    writer
        .flush()
        .with_context(|| format!("failed flushing destination file: {}", dest.display()))?;
    let inner = writer
        .into_inner()
        .map_err(|err| anyhow::anyhow!("failed to unwrap destination writer: {err}"))?;
    if fsync_enabled {
        inner
            .sync_all()
            .with_context(|| format!("failed syncing destination file: {}", dest.display()))?;
    }

    Ok(copied)
}

fn copy_stdio(source: &Path, dest: &Path, fsync_enabled: bool) -> Result<u64> {
    let mut source_file = File::open(source)
        .with_context(|| format!("failed to open source file: {}", source.display()))?;
    let mut dest_file = File::create(dest)
        .with_context(|| format!("failed to create destination file: {}", dest.display()))?;
    let copied = io::copy(&mut source_file, &mut dest_file)
        .with_context(|| format!("failed copying {} -> {}", source.display(), dest.display()))?;
    if fsync_enabled {
        dest_file
            .sync_all()
            .with_context(|| format!("failed syncing destination file: {}", dest.display()))?;
    }
    Ok(copied)
}

fn print_summary(results: &[RunResult]) {
    let average_seconds =
        results.iter().map(|result| result.seconds).sum::<f64>() / results.len() as f64;
    let average_bytes =
        results.iter().map(|result| result.bytes).sum::<u64>() / results.len() as u64;
    println!(
        "  avg   : {:>10} in {:>7.3}s  {:>12}",
        human_bytes(average_bytes),
        average_seconds,
        human_rate(average_bytes, average_seconds),
    );
}

fn print_comparison(results: &[RunResult], runs: usize) {
    let buffered_avg = average_rate(results, "buffered", runs);
    let stdio_avg = average_rate(results, "stdio", runs);
    println!("comparison:");
    println!("  buffered avg : {}", human_rate_f64(buffered_avg));
    println!("  stdio avg    : {}", human_rate_f64(stdio_avg));
    if buffered_avg > 0.0 && stdio_avg > 0.0 {
        let faster = if buffered_avg >= stdio_avg {
            "buffered"
        } else {
            "stdio"
        };
        let ratio = if buffered_avg >= stdio_avg {
            buffered_avg / stdio_avg
        } else {
            stdio_avg / buffered_avg
        };
        println!("  faster       : {faster} ({ratio:.2}x)");
    }
}

fn average_rate(results: &[RunResult], method: &str, runs: usize) -> f64 {
    let filtered: Vec<&RunResult> = results
        .iter()
        .filter(|result| result.method == method)
        .collect();
    if filtered.is_empty() || runs == 0 {
        return 0.0;
    }

    filtered
        .iter()
        .map(|result| result.bytes as f64 / result.seconds.max(f64::MIN_POSITIVE))
        .sum::<f64>()
        / filtered.len() as f64
}

fn human_bytes(size: u64) -> String {
    human_size(size as f64, &["B", "KB", "MB", "GB", "TB"])
}

fn human_rate(bytes: u64, seconds: f64) -> String {
    human_rate_f64(bytes as f64 / seconds.max(f64::MIN_POSITIVE))
}

fn human_rate_f64(rate: f64) -> String {
    human_size(rate, &["B/s", "KB/s", "MB/s", "GB/s", "TB/s"])
}

fn human_size(mut value: f64, units: &[&str]) -> String {
    let mut unit = 0;
    while value >= 1024.0 && unit < units.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{value:.0} {}", units[unit])
    } else {
        format!("{value:.1} {}", units[unit])
    }
}
