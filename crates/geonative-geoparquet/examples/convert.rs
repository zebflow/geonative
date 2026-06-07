//! Convert a single layer of a `.gdb` to a GeoParquet file.
//!
//! Usage: `convert <input.gdb> <layer-name> <output.parquet>`

use std::time::Instant;

use geonative_filegdb::open as open_gdb;
use geonative_geoparquet::{GeoParquetWriter, WriterOptions};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!("usage: {} <input.gdb> <layer-name> <output.parquet>", args[0]);
        std::process::exit(2);
    }
    let gdb_path = &args[1];
    let layer_name = &args[2];
    let out_path = &args[3];

    let gdb_size_bytes: u64 = std::fs::read_dir(gdb_path)
        .map(|d| {
            d.flatten()
                .filter_map(|e| e.metadata().ok().map(|m| m.len()))
                .sum()
        })
        .unwrap_or(0);

    let t_total = Instant::now();

    let t0 = Instant::now();
    let gdb = open_gdb(gdb_path).expect("open .gdb");
    let layer = gdb.layer(layer_name).expect("open layer");
    let expected = layer.feature_count();
    let t_open = t0.elapsed();

    println!("opened layer {layer_name:?}: {expected} features declared");
    println!("  open+schema: {:?}", t_open);

    let file = std::fs::File::create(out_path).expect("create output");
    let mut writer = GeoParquetWriter::create(file, layer.schema(), WriterOptions::default())
        .expect("create writer");

    let t_w = Instant::now();
    let mut count: u64 = 0;
    let mut last_report = Instant::now();
    for feat in layer.read() {
        let feat = feat.expect("decode feature");
        writer.write(&feat).expect("write feature");
        count += 1;
        if count % 100_000 == 0 || last_report.elapsed().as_secs() >= 5 {
            let elapsed = t_w.elapsed();
            let rate = count as f64 / elapsed.as_secs_f64();
            println!(
                "  {count:>10}/{expected} ({:.1}%) in {:.1}s — {:.0} feat/sec",
                100.0 * count as f64 / expected as f64,
                elapsed.as_secs_f64(),
                rate
            );
            last_report = Instant::now();
        }
    }
    writer.close().expect("close writer");
    let t_write = t_w.elapsed();
    let t_all = t_total.elapsed();

    let out_size = std::fs::metadata(out_path).map(|m| m.len()).unwrap_or(0);

    println!("---");
    println!("Source .gdb dir size: {}", human_bytes(gdb_size_bytes));
    println!("Output .parquet size: {}", human_bytes(out_size));
    if gdb_size_bytes > 0 {
        println!(
            "Size ratio (parquet / source): {:.2}x",
            out_size as f64 / gdb_size_bytes as f64
        );
    }
    println!("Features written: {count} (declared {expected})");
    println!(
        "Total wall time: {:.2}s; write throughput: {:.0} features/sec",
        t_all.as_secs_f64(),
        count as f64 / t_write.as_secs_f64()
    );
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
