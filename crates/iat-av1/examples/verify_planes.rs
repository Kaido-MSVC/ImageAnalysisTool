//! Stage 6 check (D-R19): decodes AVIF files with rav1d and compares every plane with the dump
//! of libavif 1.4.2 + aom (`tools/libavif_gen`), then times the decode.
//!
//!   cargo run --release -p iat-av1 --example verify_planes -- <dump-dir> <file.avif>...
//!
//! The item lookup here is the minimum for the check (primary item, its `auxl` alpha item,
//! construction method 0); the real container parser lives in iat-core.
use std::path::Path;
use std::time::Instant;
use std::{env, fs, process};

use iat_av1::{DecodeOptions, Plane};

fn be(b: &[u8], off: usize, n: usize) -> Option<u64> {
    let s = b.get(off..off.checked_add(n)?)?;
    Some(s.iter().fold(0u64, |a, &x| (a << 8) | u64::from(x)))
}

/// (type, payload start, end) of each box in `b[start..end]`.
fn boxes(b: &[u8], start: usize, end: usize) -> Vec<([u8; 4], usize, usize)> {
    let mut out = Vec::new();
    let mut p = start;
    while p + 8 <= end {
        let Some(mut size) = be(b, p, 4).and_then(|v| usize::try_from(v).ok()) else { break };
        let t: [u8; 4] = b[p + 4..p + 8].try_into().expect("4 bytes");
        let mut hdr = 8;
        if size == 1 {
            let Some(s) = be(b, p + 8, 8).and_then(|v| usize::try_from(v).ok()) else { break };
            size = s;
            hdr = 16;
        } else if size == 0 {
            size = end - p;
        }
        if size < hdr || p + size > end {
            break;
        }
        out.push((t, p + hdr, p + size));
        p += size;
    }
    out
}

struct Items {
    primary: u32,
    locs: Vec<(u32, Vec<(u64, u64)>)>,
    auxl_to: Vec<(u32, u32)>,
}

fn parse(b: &[u8]) -> Option<Items> {
    let top = boxes(b, 0, b.len());
    let &(_, ms, me) = top.iter().find(|x| &x.0 == b"meta")?;
    let mut it = Items { primary: 0, locs: Vec::new(), auxl_to: Vec::new() };
    for (t, s, e) in boxes(b, ms + 4, me) {
        match &t {
            b"pitm" => it.primary = be(b, s + 4, if b[s] == 0 { 2 } else { 4 })? as u32,
            b"iloc" => {
                let v = b[s];
                let (os, ls, bs) = (b[s + 4] >> 4, b[s + 4] & 15, b[s + 5] >> 4);
                let is = if v == 1 || v == 2 { b[s + 5] & 15 } else { 0 };
                let mut p = s + 6;
                let n = if v < 2 {
                    let n = be(b, p, 2)?;
                    p += 2;
                    n
                } else {
                    let n = be(b, p, 4)?;
                    p += 4;
                    n
                };
                for _ in 0..n {
                    let id = if v < 2 {
                        let x = be(b, p, 2)?;
                        p += 2;
                        x
                    } else {
                        let x = be(b, p, 4)?;
                        p += 4;
                        x
                    };
                    if v == 1 || v == 2 {
                        assert_eq!(be(b, p, 2)? & 15, 0, "construction method 0 only");
                        p += 2;
                    }
                    p += 2; // data_reference_index
                    let base = be(b, p, bs as usize)?;
                    p += bs as usize;
                    let ec = be(b, p, 2)?;
                    p += 2;
                    let mut ext = Vec::new();
                    for _ in 0..ec {
                        p += is as usize;
                        let o = be(b, p, os as usize)?;
                        p += os as usize;
                        let l = be(b, p, ls as usize)?;
                        p += ls as usize;
                        ext.push((base + o, l));
                    }
                    it.locs.push((id as u32, ext));
                }
            }
            b"iref" => {
                let w = if b[s] == 0 { 2 } else { 4 };
                for (rt, rs, _) in boxes(b, s + 4, e) {
                    let from = be(b, rs, w)? as u32;
                    let n = usize::try_from(be(b, rs + w, 2)?).ok()?;
                    if &rt == b"auxl" {
                        for i in 0..n {
                            it.auxl_to.push((from, be(b, rs + w + 2 + i * w, w)? as u32));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Some(it)
}

fn payload(b: &[u8], it: &Items, id: u32) -> Option<Vec<u8>> {
    let (_, ext) = it.locs.iter().find(|(i, _)| *i == id)?;
    let mut v = Vec::new();
    for &(o, l) in ext {
        v.extend_from_slice(b.get(usize::try_from(o).ok()?..usize::try_from(o + l).ok()?)?);
    }
    Some(v)
}

struct Dump {
    head: [u32; 8],
    planes: Vec<(u32, u32, Vec<u16>)>,
}

fn read_dump(p: &Path) -> Option<Dump> {
    let b = fs::read(p).ok()?;
    let u = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().expect("4 bytes"));
    let mut head = [0u32; 8];
    for (i, h) in head.iter_mut().enumerate() {
        *h = u(8 + 4 * i);
    }
    let n = u(40);
    let mut o = 44;
    let mut planes = Vec::new();
    for _ in 0..n {
        let (w, h) = (u(o), u(o + 4));
        o += 8;
        let cnt = (w * h) as usize;
        let s = b[o..o + cnt * 2].as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).collect();
        o += cnt * 2;
        planes.push((w, h, s));
    }
    Some(Dump { head, planes })
}

/// libavif `avifLimitedToFullY` (reformat.c), applied by libavif to a limited-range alpha plane.
fn limited_to_full(depth: u8, v: i32) -> i32 {
    let (lo, hi, full) = match depth {
        8 => (16, 235, 255),
        10 => (64, 940, 1023),
        _ => (256, 3760, 4095),
    };
    (((v - lo) * full + (hi - lo) / 2) / (hi - lo)).clamp(0, full)
}

fn planes_of(pic: &iat_av1::Picture, alpha: bool) -> Vec<(u32, u32, Vec<u16>)> {
    let list: &[Plane] = if alpha || !pic.has_plane(Plane::U) { &[Plane::Y] } else { &[Plane::Y, Plane::U, Plane::V] };
    list.iter()
        .map(|&pl| {
            let (w, h) = pic.plane_size(pl);
            let mut s = vec![0u16; (w * h) as usize];
            for (y, row) in s.chunks_exact_mut(w as usize).enumerate() {
                pic.read_row(pl, y as u32, row);
            }
            if alpha && !pic.full_range() {
                for v in &mut s {
                    *v = limited_to_full(pic.bit_depth(), i32::from(*v)) as u16;
                }
            }
            (w, h, s)
        })
        .collect()
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some((dump_dir, files)) = args.split_first() else {
        eprintln!("usage: verify_planes <dump-dir> <file.avif>...");
        process::exit(2);
    };
    let mut bad = 0;
    for f in files {
        let name = Path::new(f).file_name().expect("file name").to_string_lossy().into_owned();
        let b = fs::read(f).expect("read input");
        let Some(dump) = read_dump(&Path::new(dump_dir).join(format!("{name}.planes"))) else {
            println!("{name}: no libavif planes (libavif refused it), skipped");
            continue;
        };
        let it = parse(&b).expect("items");
        let color = payload(&b, &it, it.primary).expect("primary payload");
        let alpha_id = it.auxl_to.iter().find(|(_, to)| *to == it.primary).map(|(from, _)| *from);
        let opts = DecodeOptions::default();
        let runs = if b.len() > 100_000 { 5 } else { 1 };
        let mut best = f64::MAX;
        let mut got = Vec::new();
        let mut info = String::new();
        for _ in 0..runs {
            let t0 = Instant::now();
            let pic = iat_av1::decode_still(&color, &opts).expect("decode colour");
            got = planes_of(&pic, false);
            info = format!("{:?}", pic);
            if let Some(a) = alpha_id {
                let ap =
                    iat_av1::decode_still(&payload(&b, &it, a).expect("alpha payload"), &opts).expect("decode alpha");
                got.extend(planes_of(&ap, true));
            }
            best = best.min(t0.elapsed().as_secs_f64() * 1000.0);
        }
        let same = got.len() == dump.planes.len() && got.iter().zip(&dump.planes).all(|(a, b)| a == b);
        let diff: usize =
            got.iter().zip(&dump.planes).map(|(a, b)| a.2.iter().zip(&b.2).filter(|(x, y)| x != y).count()).sum();
        if !same {
            bad += 1;
        }
        println!(
            "{name}: {} planes {:?} libavif {}x{} depth {} | {info} | decode {best:.1} ms",
            if same { "SAME" } else { "DIFFERENT" },
            got.iter().map(|p| (p.0, p.1)).collect::<Vec<_>>(),
            dump.head[0],
            dump.head[1],
            dump.head[2],
        );
        if !same {
            println!("  {diff} samples differ; plane counts {} vs {}", got.len(), dump.planes.len());
        }
    }
    println!("rav1d {}: {} file(s) differ", iat_av1::rav1d_version(), bad);
    process::exit(i32::from(bad > 0));
}
