//! # geonative CLI
//!
//! Thin argument-parsing + dispatch + progress-reporting wrapper. Every
//! subcommand delegates to a library function:
//!
//! | Subcommand   | Delegates to |
//! | ------------ | ------------ |
//! | `convert`    | `geonative_convert::convert` |
//! | `inspect`    | `geonative_convert::inspect` |
//! | `metadata`   | `geonative_convert::metadata` |
//! | `filter-bbox`| `geonative_convert::filter_bbox` |
//! | `optimize`   | `geonative_geoparquet::optimize` |
//! | `profile`    | `geonative_processing::profile` (via `geonative_convert::Source`) |
//!
//! Downstream services (Zebflow, GovEyes) call those library functions
//! directly — no subprocess, no JSON parsing, no CLI dependency.

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};

use geonative_convert::{convert, filter, inspect, metadata, ConvertOptions, SinkOptions, Source};

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
    /// Convert a spatial dataset from one format to another. Accepts
    /// `.gdb`/`.shp`/`.parquet`/`.geojson` as input; `.parquet`/`.geojson`
    /// as output.
    Convert(ConvertArgs),

    /// Print a JSON report of a dataset's schema, CRS, geometry kind,
    /// declared extent, and field types.
    Inspect(InspectArgs),

    /// Rewrite a .parquet with Hilbert-sorted features and bbox covering
    /// columns. Output is functionally equivalent for queries but smaller
    /// and faster to scan with a bbox predicate.
    Optimize(OptimizeArgs),

    /// Stream-filter features by bbox intersection and write the survivors.
    /// Coarse filter only — uses each feature's bbox, not exact geometry.
    #[command(name = "filter-bbox")]
    FilterBbox(FilterBboxArgs),

    /// Write a `.geonative.json` sidecar describing a dataset.
    Metadata(MetadataArgs),

    /// Profile a dataset: streaming pass producing null counts, min/max,
    /// distinct counts, top-N values, head sample, computed extent.
    Profile(ProfileArgs),

    /// Reproject features to a target CRS (4326/3857/UTM/MGA) and write to
    /// a new file. Pure-Rust Krüger n-series; nanometre precision in zone.
    Reproject(ReprojectArgs),
}

#[derive(Parser, Debug)]
struct ConvertArgs {
    /// Input dataset.
    input: PathBuf,
    /// Output file.
    output: PathBuf,
    /// Layer name to convert (required for multi-layer FileGDB inputs).
    #[arg(short, long)]
    layer: Option<String>,
    /// Reproject every feature's geometry to this CRS mid-stream.
    /// Accepted forms: `EPSG:4326`, `4326`. v0.1 supports 4326, 3857,
    /// 7844, UTM (32601–32760), GDA2020/MGA (7846–7859).
    #[arg(long = "to-crs")]
    to_crs: Option<String>,
    /// Buffer all features and Hilbert-sort by bbox centroid before writing
    /// (parquet output only). Spatially clusters the file; memory ∝ feature count.
    #[arg(long)]
    hilbert: bool,
    /// Rows per parquet row group.
    #[arg(long, default_value_t = 10_000)]
    batch_size: usize,
    /// Skip the bbox covering columns + GeoParquet `covering` metadata.
    #[arg(long)]
    no_bbox_columns: bool,
}

#[derive(Parser, Debug)]
struct ReprojectArgs {
    /// Input dataset.
    input: PathBuf,
    /// Output file.
    output: PathBuf,
    /// Target CRS: `EPSG:4326` or `4326`.
    #[arg(long = "to-crs")]
    to_crs: String,
    /// Layer name (required for multi-layer FileGDB inputs).
    #[arg(short, long)]
    layer: Option<String>,
    #[arg(long, default_value_t = 10_000)]
    batch_size: usize,
}

#[derive(Parser, Debug)]
struct InspectArgs {
    input: PathBuf,
    #[arg(long)]
    pretty: bool,
}

#[derive(Parser, Debug)]
struct OptimizeArgs {
    input: PathBuf,
    output: PathBuf,
    #[arg(long, default_value_t = 10_000)]
    batch_size: usize,
}

#[derive(Parser, Debug)]
struct FilterBboxArgs {
    input: PathBuf,
    output: PathBuf,
    /// Query bbox, in the input's native CRS: `xmin,ymin,xmax,ymax`.
    #[arg(long)]
    bbox: String,
    #[arg(short, long)]
    layer: Option<String>,
    #[arg(long, default_value_t = 10_000)]
    batch_size: usize,
}

#[derive(Parser, Debug)]
struct ProfileArgs {
    input: PathBuf,
    #[arg(short, long)]
    layer: Option<String>,
    #[arg(long, default_value_t = 10)]
    top: usize,
    #[arg(long, default_value_t = 5)]
    sample: usize,
    #[arg(long, default_value_t = 10_000)]
    distinct_limit: usize,
    #[arg(long)]
    pretty: bool,
}

#[derive(Parser, Debug)]
struct MetadataArgs {
    input: PathBuf,
    /// Where to write the sidecar. Defaults to `<input>.geonative.json`.
    #[arg(long)]
    write: Option<PathBuf>,
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
        Cmd::Profile(args) => run_profile(args),
        Cmd::Reproject(args) => run_reproject(args),
    }
}

fn run_convert(args: ConvertArgs) -> Result<(), String> {
    let start = std::time::Instant::now();
    let to_crs = match args.to_crs {
        Some(s) => Some(parse_crs(&s)?),
        None => None,
    };
    let opts = ConvertOptions {
        layer: args.layer,
        sink: SinkOptions {
            batch_size: args.batch_size,
            add_bbox_columns: !args.no_bbox_columns,
            hilbert_sort: args.hilbert,
            ..SinkOptions::default()
        },
        to_crs,
        progress: Some(Arc::new(move |written, expected| {
            let elapsed = start.elapsed().as_secs_f64().max(0.001);
            let pct = if expected > 0 {
                format!("{:.1}%", 100.0 * written as f64 / expected as f64)
            } else {
                "—".to_string()
            };
            eprintln!(
                "  {written}/{expected} ({pct}) — {:.0} feat/sec",
                written as f64 / elapsed
            );
        })),
    };
    let stats = convert(&args.input, &args.output, opts).map_err(|e| e.to_string())?;
    eprintln!(
        "wrote {} feature{plural} to {} ({}) in {:.2}s @ {:.0} feat/sec",
        stats.features,
        args.output.display(),
        human_bytes(stats.output_bytes),
        stats.elapsed_secs,
        stats.features as f64 / stats.elapsed_secs.max(0.001),
        plural = if stats.features == 1 { "" } else { "s" }
    );
    Ok(())
}

fn run_inspect(args: InspectArgs) -> Result<(), String> {
    let report = inspect(&args.input).map_err(|e| e.to_string())?;
    print_json(&report, args.pretty)
}

fn run_optimize(args: OptimizeArgs) -> Result<(), String> {
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
    let report = geonative_geoparquet::optimize(
        &args.input,
        &args.output,
        geonative_geoparquet::OptimizeOptions {
            batch_size: args.batch_size,
        },
    )
    .map_err(|e| e.to_string())?;
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
    let bbox = parse_bbox(&args.bbox)?;
    let stats = filter::filter_bbox(
        &args.input,
        &args.output,
        bbox,
        args.layer.as_deref(),
        SinkOptions {
            batch_size: args.batch_size,
            add_bbox_columns: true,
            hilbert_sort: false,
            ..SinkOptions::default()
        },
    )
    .map_err(|e| e.to_string())?;
    let pct = if stats.scanned > 0 {
        100.0 * stats.kept as f64 / stats.scanned as f64
    } else {
        0.0
    };
    eprintln!(
        "scanned {} features, kept {} ({:.1}%) in {:.2}s",
        stats.scanned, stats.kept, pct, stats.elapsed_secs
    );
    Ok(())
}

fn run_metadata(args: MetadataArgs) -> Result<(), String> {
    let sidecar = metadata(&args.input).map_err(|e| e.to_string())?;
    let target = args
        .write
        .unwrap_or_else(|| geonative_convert::default_sidecar_path(&args.input));
    let bytes = if args.pretty {
        serde_json::to_vec_pretty(&sidecar)
    } else {
        serde_json::to_vec(&sidecar)
    }
    .map_err(|e| format!("serialising sidecar: {e}"))?;
    std::fs::write(&target, bytes).map_err(|e| format!("writing {}: {e}", target.display()))?;
    eprintln!("wrote sidecar to {}", target.display());
    Ok(())
}

fn run_reproject(args: ReprojectArgs) -> Result<(), String> {
    let target = parse_crs(&args.to_crs)?;
    let opts = ConvertOptions {
        layer: args.layer,
        sink: SinkOptions {
            batch_size: args.batch_size,
            add_bbox_columns: true,
            hilbert_sort: false,
            ..SinkOptions::default()
        },
        to_crs: Some(target),
        progress: None,
    };
    let stats =
        geonative_convert::convert(&args.input, &args.output, opts).map_err(|e| e.to_string())?;
    eprintln!(
        "reprojected {} features in {:.2}s — {}",
        stats.features,
        stats.elapsed_secs,
        human_bytes(stats.output_bytes),
    );
    Ok(())
}

fn parse_crs(s: &str) -> Result<geonative_core::Crs, String> {
    let trimmed = s.trim();
    let digits = trimmed
        .strip_prefix("EPSG:")
        .or_else(|| trimmed.strip_prefix("epsg:"))
        .unwrap_or(trimmed);
    let code: u32 = digits
        .parse()
        .map_err(|e| format!("--to-crs expects EPSG:NNNN or NNNN, got '{s}': {e}"))?;
    Ok(geonative_core::Crs::Epsg(code))
}

fn run_profile(args: ProfileArgs) -> Result<(), String> {
    let source = Source::open(&args.input, args.layer.as_deref()).map_err(|e| e.to_string())?;
    let schema = source.schema_cloned().map_err(|e| e.to_string())?;
    let mut features: Vec<geonative_core::Feature> = Vec::new();
    source
        .for_each(|f| {
            features.push(f);
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    let opts = geonative_processing::ProfileOptions {
        top_n: args.top,
        sample_n: args.sample,
        distinct_limit: args.distinct_limit,
    };
    let report = geonative_processing::profile(&schema, features, opts);
    print_json(&report, args.pretty)
}

fn parse_bbox(s: &str) -> Result<[f64; 4], String> {
    let parts: Vec<&str> = s.split(',').map(str::trim).collect();
    if parts.len() != 4 {
        return Err(format!(
            "--bbox expects 4 comma-separated numbers (xmin,ymin,xmax,ymax), got: {s}"
        ));
    }
    let mut out = [0.0_f64; 4];
    for (i, p) in parts.iter().enumerate() {
        out[i] = p
            .parse::<f64>()
            .map_err(|e| format!("--bbox component {i} ('{p}'): {e}"))?;
    }
    if out[0] > out[2] || out[1] > out[3] {
        return Err(format!(
            "--bbox is degenerate (xmin>xmax or ymin>ymax): [{}, {}, {}, {}]",
            out[0], out[1], out[2], out[3]
        ));
    }
    Ok(out)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bbox_ok() {
        assert_eq!(parse_bbox("1,2,3,4").unwrap(), [1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn parse_bbox_with_spaces() {
        assert_eq!(parse_bbox(" 1 , 2 , 3 , 4 ").unwrap(), [1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn parse_bbox_wrong_count() {
        assert!(parse_bbox("1,2,3").is_err());
        assert!(parse_bbox("1,2,3,4,5").is_err());
    }

    #[test]
    fn parse_bbox_degenerate() {
        assert!(parse_bbox("3,2,1,4").is_err());
        assert!(parse_bbox("1,4,3,2").is_err());
    }
}
