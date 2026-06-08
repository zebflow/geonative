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
mod io;
mod metadata;
mod optimize;

use std::path::PathBuf;
use std::time::Instant;

use clap::{Parser, Subcommand};

use crate::io::{Sink, SinkOptions, Source};

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

fn run_convert(args: ConvertArgs) -> Result<(), String> {
    let source = Source::open(&args.input, args.layer.as_deref())?;
    let schema = source.schema_cloned()?;
    let expected = source.feature_count().unwrap_or(0);

    let mut sink = Sink::create(
        &args.output,
        &schema,
        SinkOptions {
            batch_size: args.batch_size,
            add_bbox_columns: !args.no_bbox_columns,
            hilbert_sort: args.hilbert,
        },
    )?;

    let start = Instant::now();
    let mut last_report = Instant::now();
    let mut count: u64 = 0;
    source.for_each(|feat| {
        sink.write(&feat)?;
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
        Ok(())
    })?;
    sink.close()?;

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

    let source = Source::open(&args.input, args.layer.as_deref())?;
    let schema = source.schema_cloned()?;
    let mut sink = Sink::create(
        &args.output,
        &schema,
        SinkOptions {
            batch_size: args.batch_size,
            add_bbox_columns: true,
            hilbert_sort: false,
        },
    )?;

    let start = Instant::now();
    let mut scanned: u64 = 0;
    let mut kept: u64 = 0;
    source.for_each(|feat| {
        scanned += 1;
        if let Some(geom) = feat.geometry.as_ref() {
            if let Some(fb) = geom.bbox() {
                if filter_bbox::bbox_intersects(fb, bbox) {
                    sink.write(&feat)?;
                    kept += 1;
                }
            }
        }
        Ok(())
    })?;
    sink.close()?;

    let elapsed = start.elapsed().as_secs_f64();
    let pct = if scanned > 0 {
        100.0 * kept as f64 / scanned as f64
    } else {
        0.0
    };
    eprintln!("scanned {scanned} features, kept {kept} ({pct:.1}%) in {elapsed:.2}s");
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
