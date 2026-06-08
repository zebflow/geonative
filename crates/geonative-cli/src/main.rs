//! # geonative CLI
//!
//! Single binary, subcommand-style — same shape as `git`, `cargo`, `gh`.
//!
//! Subcommands:
//!
//! - `convert <input> <output> [options]` — convert a spatial dataset from
//!   one format to another. v0.1 supports `.gdb → .parquet`.
//! - `inspect <source> [--pretty]` — emit JSON describing schema, CRS,
//!   geometry kind, declared extent, and field types.
//! - `optimize <input.parquet> <output.parquet>` — rewrite a parquet with
//!   Hilbert-sorted features and bbox covering columns.
//! - `filter-bbox <input> <output.parquet> --bbox xmin,ymin,xmax,ymax` —
//!   stream-filter features by bbox intersection.
//! - `metadata <source> [--write PATH] [--pretty]` — write a
//!   `.geonative.json` sidecar describing the dataset.
//!
//! Architecture note: the CLI is a thin orchestration layer — every line
//! of geospatial logic lives in the library crates. This file is mainly
//! about argument parsing + dispatch + user-friendly error messages.

mod filter_bbox;
mod inspect;
mod metadata;
mod optimize;

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
    propagate_version = true
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

    /// Print a JSON report of a dataset's schema, CRS, geometry kind,
    /// declared extent, and field types.
    Inspect(InspectArgs),

    /// Rewrite a .parquet with Hilbert-sorted features and bbox covering
    /// columns. Output is functionally equivalent for queries but smaller
    /// and faster to scan with a bbox predicate.
    Optimize(OptimizeArgs),

    /// Stream-filter features by bbox intersection and write the survivors
    /// to a .parquet. Coarse filter only — uses each feature's bbox, not
    /// exact geometry.
    #[command(name = "filter-bbox")]
    FilterBbox(FilterBboxArgs),

    /// Write a `.geonative.json` sidecar describing a dataset. Composes
    /// `inspect` plus a generator envelope. The sidecar is consumed by
    /// downstream tooling that wants schema/CRS info without re-parsing
    /// the source file.
    Metadata(MetadataArgs),
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

#[derive(Parser, Debug)]
struct InspectArgs {
    /// Input dataset (`.gdb`, `.shp`, or `.parquet`).
    input: PathBuf,

    /// Pretty-print the JSON (2-space indent) instead of compact.
    #[arg(long)]
    pretty: bool,
}

#[derive(Parser, Debug)]
struct OptimizeArgs {
    /// Input `.parquet` file.
    input: PathBuf,

    /// Output `.parquet` file.
    output: PathBuf,

    /// Rows per parquet row group. Default 10000.
    #[arg(long, default_value_t = 10_000)]
    batch_size: usize,
}

#[derive(Parser, Debug)]
struct FilterBboxArgs {
    /// Input dataset (`.gdb`, `.shp`, or `.parquet`).
    input: PathBuf,

    /// Output `.parquet` file.
    output: PathBuf,

    /// Query bbox, in the input's native CRS: `xmin,ymin,xmax,ymax`.
    #[arg(long)]
    bbox: String,

    /// Layer name (required for multi-layer `.gdb` inputs).
    #[arg(short, long)]
    layer: Option<String>,

    /// Rows per parquet row group. Default 10000.
    #[arg(long, default_value_t = 10_000)]
    batch_size: usize,
}

#[derive(Parser, Debug)]
struct MetadataArgs {
    /// Input dataset (`.gdb`, `.shp`, or `.parquet`).
    input: PathBuf,

    /// Where to write the sidecar. Defaults to `<input>.geonative.json`.
    #[arg(long)]
    write: Option<PathBuf>,

    /// Pretty-print the JSON (2-space indent).
    #[arg(long)]
    pretty: bool,
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
        Cmd::Inspect(args) => run_inspect(args),
        Cmd::Optimize(args) => run_optimize(args),
        Cmd::FilterBbox(args) => run_filter_bbox(args),
        Cmd::Metadata(args) => run_metadata(args),
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
            "unsupported input extension '.{other}' (convert v0.1 supports: .gdb)"
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

fn run_convert(args: ConvertArgs) -> Result<(), String> {
    let input_kind = detect_input(&args.input)?;
    let output_kind = detect_output(&args.output)?;

    match (input_kind, output_kind) {
        (InputKind::FileGdb, OutputKind::GeoParquet) => convert_gdb_to_parquet(args),
    }
}

fn convert_gdb_to_parquet(args: ConvertArgs) -> Result<(), String> {
    let gdb =
        open_gdb(&args.input).map_err(|e| format!("opening {}: {e}", args.input.display()))?;
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
    writer.close().map_err(|e| format!("closing writer: {e}"))?;

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

fn run_inspect(args: InspectArgs) -> Result<(), String> {
    let report = inspect::inspect(&args.input)?;
    print_json(&report, args.pretty)
}

fn run_optimize(args: OptimizeArgs) -> Result<(), String> {
    // Sanity-check the extensions before touching disk so users get a fast,
    // clear error instead of a parquet-format failure deep in the pipeline.
    if args.input.extension().and_then(|s| s.to_str()) != Some("parquet") {
        return Err(format!(
            "optimize input must be a .parquet file: {}",
            args.input.display()
        ));
    }
    if args.output.extension().and_then(|s| s.to_str()) != Some("parquet") {
        return Err(format!(
            "optimize output must be a .parquet file: {}",
            args.output.display()
        ));
    }

    let report = optimize::optimize(optimize::OptimizeArgs {
        input: &args.input,
        output: &args.output,
        batch_size: args.batch_size,
    })?;

    let ratio = if report.input_bytes > 0 {
        100.0 * report.output_bytes as f64 / report.input_bytes as f64
    } else {
        0.0
    };
    eprintln!(
        "optimized {} features in {:.2}s — {} → {} ({:.1}% of input)",
        report.features,
        report.elapsed_secs,
        human_bytes(report.input_bytes),
        human_bytes(report.output_bytes),
        ratio
    );
    Ok(())
}

fn run_filter_bbox(args: FilterBboxArgs) -> Result<(), String> {
    let bbox = filter_bbox::parse_bbox(&args.bbox)?;
    if args.output.extension().and_then(|s| s.to_str()) != Some("parquet") {
        return Err(format!(
            "filter-bbox output must be a .parquet file: {}",
            args.output.display()
        ));
    }

    let report = filter_bbox::filter_bbox(filter_bbox::FilterBboxArgs {
        input: &args.input,
        output: &args.output,
        bbox,
        layer: args.layer.as_deref(),
        batch_size: args.batch_size,
    })?;
    let pct = if report.scanned > 0 {
        100.0 * report.kept as f64 / report.scanned as f64
    } else {
        0.0
    };
    eprintln!(
        "scanned {} features, kept {} ({:.1}%) in {:.2}s",
        report.scanned, report.kept, pct, report.elapsed_secs
    );
    Ok(())
}

fn run_metadata(args: MetadataArgs) -> Result<(), String> {
    let sidecar = metadata::build(&args.input)?;
    let target = args
        .write
        .unwrap_or_else(|| metadata::default_sidecar_path(&args.input));

    let bytes = if args.pretty {
        serde_json::to_vec_pretty(&sidecar)
    } else {
        serde_json::to_vec(&sidecar)
    }
    .map_err(|e| format!("serialising sidecar: {e}"))?;

    std::fs::write(&target, bytes)
        .map_err(|e| format!("writing {}: {e}", target.display()))?;
    eprintln!("wrote sidecar to {}", target.display());
    Ok(())
}

fn print_json<T: serde::Serialize>(value: &T, pretty: bool) -> Result<(), String> {
    let s = if pretty {
        serde_json::to_string_pretty(value)
    } else {
        serde_json::to_string(value)
    }
    .map_err(|e| format!("serialising JSON: {e}"))?;
    println!("{s}");
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
