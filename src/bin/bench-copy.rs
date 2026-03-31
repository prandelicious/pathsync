use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

#[path = "../copy_fast_path.rs"]
mod copy_fast_path;

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

    #[arg(long, value_enum, default_value_t = Method::All)]
    method: Method,

    #[arg(long, default_value_t = true)]
    fsync: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum Method {
    Buffered,
    Stdio,
    Native,
    Both,
    All,
}

impl Method {
    fn label(self) -> &'static str {
        match self {
            Method::Buffered => "buffered",
            Method::Stdio => "stdio",
            Method::Native => "native",
            Method::Both => "both",
            Method::All => "all",
        }
    }
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

    for method in &methods {
        let method_name = method.label();
        println!("method: {method_name}");
        let mut method_results = Vec::new();

        for run in 1..=cli.runs {
            let destination = temp_destination(&cli.target_dir, method_name, run);
            let started = Instant::now();
            let bytes = run_method(*method, &cli.source, &destination, buffer_size, cli.fsync)?;
            let elapsed = started.elapsed().as_secs_f64();

            fs::remove_file(&destination).with_context(|| {
                format!(
                    "failed to remove benchmark temp file: {}",
                    destination.display()
                )
            })?;

            let result = RunResult {
                method: method_name,
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

    if methods.len() > 1 {
        print_comparison(&all_results, &methods);
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

fn selected_methods(method: Method) -> Vec<Method> {
    match method {
        Method::Buffered => vec![Method::Buffered],
        Method::Stdio => vec![Method::Stdio],
        Method::Native => vec![Method::Native],
        Method::Both => vec![Method::Buffered, Method::Stdio],
        Method::All => vec![Method::Native, Method::Buffered, Method::Stdio],
    }
}

fn temp_destination(target_dir: &Path, method: &str, run: usize) -> PathBuf {
    target_dir.join(format!(".pathsync-bench-{method}-{run}.tmp"))
}

fn run_method(
    method: Method,
    source: &Path,
    dest: &Path,
    buffer_size: usize,
    fsync_enabled: bool,
) -> Result<u64> {
    match method {
        Method::Buffered => copy_buffered(source, dest, buffer_size, fsync_enabled),
        Method::Stdio => copy_stdio(source, dest, fsync_enabled),
        Method::Native => copy_native(source, dest, fsync_enabled),
        Method::Both => unreachable!("both is a selector, not a benchmark mode"),
        Method::All => unreachable!("all is a selector, not a benchmark mode"),
    }
}

fn copy_native(source: &Path, dest: &Path, fsync_enabled: bool) -> Result<u64> {
    let outcome = copy_fast_path::copy_file_data(source, dest, |_| {}).map_err(|err| {
        anyhow::anyhow!(
            "failed native copying {} -> {}: {err:?}",
            source.display(),
            dest.display()
        )
    })?;

    if fsync_enabled {
        File::open(dest)
            .with_context(|| format!("failed to open destination file: {}", dest.display()))?
            .sync_all()
            .with_context(|| format!("failed syncing destination file: {}", dest.display()))?;
    }

    Ok(outcome.bytes)
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

fn print_comparison(results: &[RunResult], methods: &[Method]) {
    println!("comparison:");
    let mut rates = Vec::new();
    for method in methods {
        let label = method.label();
        let rate = average_rate(results, label);
        println!("  {label:<12}: {}", human_rate_f64(rate));
        if rate > 0.0 {
            rates.push((label, rate));
        }
    }
    rates.sort_by(|a, b| b.1.total_cmp(&a.1));
    if let Some((faster, fastest_rate)) = rates.first().copied()
        && let Some((_, next_best_rate)) = rates.get(1).copied()
    {
        println!(
            "  faster       : {faster} ({:.2}x)",
            fastest_rate / next_best_rate
        );
    }
}

#[cfg(test)]
fn fastest_rate<'a>(results: &'a [(&'a str, f64)]) -> Option<(&'a str, f64)> {
    let mut iter = results.iter().copied();
    let first = iter.next()?;
    Some(iter.fold(first, |best, candidate| {
        if candidate.1 > best.1 {
            candidate
        } else {
            best
        }
    }))
}

fn average_rate(results: &[RunResult], method: &str) -> f64 {
    let filtered: Vec<&RunResult> = results
        .iter()
        .filter(|result| result.method == method)
        .collect();
    if filtered.is_empty() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(prefix: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "pathsync-bench-copy-{prefix}-{}-{unique}",
            std::process::id()
        ))
    }

    fn write_file(path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("failed to create parent directory");
        }
        let mut file = File::create(path).expect("failed to create file");
        file.write_all(contents).expect("failed to write file");
    }

    #[test]
    fn method_selection_includes_native_for_all() {
        assert_eq!(
            selected_methods(Method::Both),
            vec![Method::Buffered, Method::Stdio]
        );
        assert_eq!(
            selected_methods(Method::All),
            vec![Method::Native, Method::Buffered, Method::Stdio]
        );
        assert_eq!(selected_methods(Method::Native), vec![Method::Native]);
    }

    #[test]
    fn cli_defaults_to_all() {
        let root = temp_dir("cli-default");
        let source_dir = root.join("source");
        let target_dir = root.join("target");
        fs::create_dir_all(&source_dir).expect("failed to create source dir");
        fs::create_dir_all(&target_dir).expect("failed to create target dir");
        let source = source_dir.join("source.bin");
        write_file(&source, b"default");

        let cli = Cli::try_parse_from([
            "bench-copy",
            "--source",
            source.to_str().expect("source path utf8"),
            "--target-dir",
            target_dir.to_str().expect("target path utf8"),
        ])
        .expect("cli parse should succeed");

        assert_eq!(cli.method, Method::All);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn native_copy_uses_production_backend() {
        let root = temp_dir("native-copy");
        let source_dir = root.join("source");
        let target_dir = root.join("target");
        fs::create_dir_all(&source_dir).expect("failed to create source dir");
        fs::create_dir_all(&target_dir).expect("failed to create target dir");
        let source = source_dir.join("source.bin");
        let dest = target_dir.join("dest.bin");
        let payload = b"native-bench-bytes";
        write_file(&source, payload);

        let copied = copy_native(&source, &dest, false).expect("native copy should succeed");
        assert_eq!(copied, payload.len() as u64);
        assert_eq!(
            fs::read(&dest).expect("failed to read destination"),
            payload
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn average_rate_handles_empty_input() {
        assert_eq!(average_rate(&[], "buffered"), 0.0);
        assert_eq!(average_rate(&[], "native"), 0.0);
    }

    #[test]
    fn comparison_helpers_pick_fastest_method() {
        let results = vec![
            RunResult {
                method: "native",
                run: 1,
                bytes: 10,
                seconds: 1.0,
            },
            RunResult {
                method: "buffered",
                run: 1,
                bytes: 10,
                seconds: 2.0,
            },
            RunResult {
                method: "stdio",
                run: 1,
                bytes: 10,
                seconds: 4.0,
            },
        ];

        assert!(average_rate(&results, "native") > average_rate(&results, "buffered"));
        assert!(average_rate(&results, "buffered") > average_rate(&results, "stdio"));
        assert_eq!(
            fastest_rate(&[("native", 10.0), ("buffered", 4.0), ("stdio", 1.0)]),
            Some(("native", 10.0))
        );
    }

    #[test]
    fn human_rate_formats_rates() {
        assert!(human_rate_f64(1_500_000.0).contains("MB/s"));
    }
}
