//! 檢視解碼的計時：`decode_for_view` 對一張大圖執行數次，印出每次的耗時。
//!
//!   cargo run --release -p iat-core --example view_timing -- <image> [次數]
//!   cargo run --release -p iat-core --example view_timing -- --make <out.png> <寬> <高>
//!
//! 走 worker 的格式需要 `iat-worker.exe` 放在這個範例執行檔旁邊（target\release\examples）。
use std::path::Path;
use std::time::Instant;
use std::{env, fs, io, process};

use iat_core::{CancelToken, Limits, decode_for_view};

/// 產生 RGB8 PNG：漸層加上決定性的雜訊，壓縮率接近一般照片。
fn make_png(out: &Path, w: u32, h: u32) -> io::Result<()> {
    let f = io::BufWriter::new(fs::File::create_new(out)?);
    let mut enc = png::Encoder::new(f, w, h);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    let mut wr = enc.write_header().map_err(io::Error::other)?;
    let mut stream = wr.stream_writer().map_err(io::Error::other)?;
    let mut row = vec![0u8; w as usize * 3];
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    for y in 0..h {
        for (x, px) in (0u32..).zip(row.chunks_exact_mut(3)) {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            let noise = (seed >> 60).to_le_bytes()[0];
            px[0] = (x >> 6).to_le_bytes()[0].wrapping_add(noise);
            px[1] = (y >> 5).to_le_bytes()[0].wrapping_add(noise);
            px[2] = ((x ^ y) >> 7).to_le_bytes()[0];
        }
        io::Write::write_all(&mut stream, &row)?;
    }
    stream.finish().map_err(io::Error::other)
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--make") {
        let [_, out, w, h] = args.as_slice() else {
            eprintln!("usage: --make <out.png> <width> <height>");
            process::exit(2);
        };
        let (Ok(w), Ok(h)) = (w.parse(), h.parse()) else {
            eprintln!("width and height must be numbers");
            process::exit(2);
        };
        if let Err(e) = make_png(Path::new(out), w, h) {
            eprintln!("cannot write {out}: {e}");
            process::exit(1);
        }
        return;
    }
    let Some(path) = args.first() else {
        eprintln!("usage: view_timing <image> [runs] | --make <out.png> <width> <height>");
        process::exit(2);
    };
    let runs: u32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(3);
    for i in 1..=runs {
        let t0 = Instant::now();
        match decode_for_view(Path::new(path), None, &Limits::default(), 1 << 28, &CancelToken::new()) {
            Ok(img) => println!(
                "run {i}: {:.0} ms ({}x{} {} {}-bit, {} rows)",
                t0.elapsed().as_secs_f64() * 1000.0,
                img.width,
                img.height,
                img.model,
                img.bits,
                img.rows
            ),
            Err(e) => {
                eprintln!("run {i}: {e}");
                process::exit(1);
            }
        }
    }
}
