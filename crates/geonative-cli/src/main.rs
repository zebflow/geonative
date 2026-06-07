//! # geonative CLI
//!
//! Single binary, subcommand-style — same shape as `git`, `cargo`, `gh`.
//!
//! Currently exposes one subcommand:
//!
//! - `geonative convert <input> <output> [options]` — convert a spatial
//!   dataset from one format to another. v0.1 supports `.gdb → .parquet`
//!   via [`geonative_filegdb`] + [`geonative_geoparquet`]. Format detection
//!   is by file extension; mismatches produce a clear error rather than
//!   silently doing the wrong thing.
//!
//! Architecture note: the CLI is a thin orchestration layer — every line
//! of geospatial logic lives in the library crates. This file is mainly
//! about argument parsing + dispatch + user-friendly error messages.

use std::path::{Path, PathBuf};
use std::time::Instant;

use clap::{Parser, Subcommand};
use geonative_filegdb::open as open_gdb;
use geonative_geoparquet::{GeoParquetWriter, WriterOptions};

#[derive(Parser, Debug)]
#[command(
    name = "geonative",
    version,
    about = "Pure-Rust geospatial toolkit",
    propagate_version = true,
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Convert a spatial dataset from one format to another.
    ///
    /// Format is detected by extension. v0.1 supports `.gdb → .parquet`.
    Convert(ConvertArgs),
}

#[derive(Parser, Debug)]
struct ConvertArgs {
    /// Input dataset (e.g. `path/to/foo.gdb`).
    input: PathBuf,

    /// Output file (e.g. `out.parquet`).
    output: PathBuf,

    /// Layer name to convert (required when input has multiple layers; if
    /// omitted and exactly one user layer exists, that one is used).
    #[arg(short, long)]
    layer: Option<String>,

    /// Buffer all features and Hilbert-sort by bbox centroid before writing.
    /// Produces a smaller, spatially-clustered parquet — better for predicate
    /// pushdown reads. Uses memory proportional to feature count.
    #[arg(long)]
    hilbert: bool,

    /// Rows per parquet row group. Default 10000.
    #[arg(long, default_value_t = 10_000)]
    batch_size: usize,

    /// Skip the bbox covering columns (xmin/ymin/xmax/ymax + GeoParquet
    /// `covering` metadata).
    #[arg(long)]
    no_bbox_columns: bool,
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.cmd {
        Cmd::Convert(args) => run_convert(args),
    }
}

fn run_convert(args: ConvertArgs) -> Result<(), String> {
    let input_kind = detect_input(&args.input)?;
    let output_kind = detect_output(&args.output)?;

    match (input_kind, output_kind) {
        (InputKind::FileGdb, OutputKind::GeoParquet) => convert_gdb_to_parquet(args),
    }
}

#[derive(Debug, Clone, Copy)]
enum InputKind {
    FileGdb,
}

#[derive(Debug, Clone, Copy)]
enum OutputKind {
    GeoParquet,
}

fn detect_input(path: &Path) -> Result<InputKind, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("gdb") => Ok(InputKind::FileGdb),
        Some(other) => Err(format!(
            "unsupported input extension '.{other}' (v0.1 supports: .gdb)"
        )),
        None => Err(format!(
            "could not determine input format from path: {} (extension required)",
            path.display()
        )),
    }
}

fn detect_output(path: &Path) -> Result<OutputKind, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("parquet") => Ok(OutputKind::GeoParquet),
        Some(other) => Err(format!(
            "unsupported output extension '.{other}' (v0.1 supports: .parquet)"
        )),
        None => Err(format!(
            "could not determine output format from path: {} (extension required)",
            path.display()
        )),
    }
}

fn convert_gdb_to_parquet(args: ConvertArgs) -> Result<(), String> {
    let gdb = open_gdb(&args.input)
        .map_err(|e| format!("opening {}: {e}", args.input.display()))?;
    let layers = gdb.layers();

    let layer_name = match (&args.layer, layers) {
        (Some(name), _) => name.clone(),
        (None, [single]) => single.name.clone(),
        (None, many) => {
            return Err(format!(
                "input has {} layers; specify which with --layer NAME. Available: {}",
                many.len(),
                many.iter()
                    .map(|l| l.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        }
    };

    let layer = gdb
        .layer(&layer_name)
        .map_err(|e| format!("opening layer '{layer_name}': {e}"))?;
    let expected = layer.feature_count();

    let writer_opts = WriterOptions {
        batch_size: args.batch_size,
        add_bbox_columns: !args.no_bbox_columns,
        hilbert_sort: args.hilbert,
        ..WriterOptions::default()
    };

    let file = std::fs::File::create(&args.output)
        .map_err(|e| format!("creating {}: {e}", args.output.display()))?;
    let mut writer = GeoParquetWriter::create(file, layer.schema(), writer_opts)
        .map_err(|e| format!("creating writer: {e}"))?;

    let start = Instant::now();
    let mut last_report = Instant::now();
    let mut count: u64 = 0;
    for feat in layer.read() {
        let feat = feat.map_err(|e| format!("decoding feature {count}: {e}"))?;
        writer
            .write(&feat)
            .map_err(|e| format!("writing feature {count}: {e}"))?;
        count += 1;
        if last_report.elapsed().as_secs() >= 2 {
            let pct = if expected > 0 {
                100.0 * count as f64 / expected as f64
            } else {
                0.0
            };
            eprintln!(
                "  {count}/{expected} ({pct:.1}%) — {:.0} feat/sec",
                count as f64 / start.elapsed().as_secs_f64()
            );
            last_report = Instant::now();
        }
    }
    writer
        .close()
        .map_err(|e| format!("closing writer: {e}"))?;

    let elapsed = start.elapsed();
    let out_size = std::fs::metadata(&args.output)
        .map(|m| m.len())
        .unwrap_or(0);
    eprintln!(
        "wrote {count} feature{plural} to {} ({}) in {:.2}s @ {:.0} feat/sec",
        args.output.display(),
        human_bytes(out_size),
        elapsed.as_secs_f64(),
        count as f64 / elapsed.as_secs_f64(),
        plural = if count == 1 { "" } else { "s" }
    );
    Ok(())
}

fn human_bytes(n: u64) -> String {
    let f = n as f64;
    if f < 1024.0 {
        format!("{n} B")
    } else if f < 1024.0 * 1024.0 {
        format!("{:.1} KB", f / 1024.0)
    } else if f < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} MB", f / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", f / (1024.0 * 1024.0 * 1024.0))
    }
}
