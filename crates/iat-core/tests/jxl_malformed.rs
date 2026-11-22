//! 畸形 JPEG XL 容器與 codestream：掃描與解碼都必須回報錯誤，不可以 panic 或溢位（D-R18）。

use std::path::{Path, PathBuf};

use iat_core::input::Input;
use iat_core::jxl_scan::scan_jxl;
use iat_core::{CancelToken, Limits, decode_for_view};

fn write_temp(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join("iat-core-jxl-malformed");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let p = dir.join(name);
    std::fs::write(&p, bytes).expect("write synthetic file");
    p
}

fn scan(name: &str, bytes: &[u8]) -> iat_core::jxl_scan::JxlScan {
    let p = write_temp(name, bytes);
    let f = std::fs::File::open(&p).expect("open");
    let input = Input::new(&f).expect("input");
    scan_jxl(&input, &Limits::default())
}

fn first(s: &iat_core::jxl_scan::JxlScan) -> (&str, &str) {
    let i = s.issues.first().expect("an issue");
    (i.code.as_str(), i.message.as_str())
}

fn boxed(ty: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut b = u32::try_from(payload.len() + 8).expect("box size fits in u32").to_be_bytes().to_vec();
    b.extend_from_slice(ty);
    b.extend_from_slice(payload);
    b
}

fn container(rest: &[u8]) -> Vec<u8> {
    let mut f = b"\0\0\0\x0CJXL \r\n\x87\n".to_vec();
    f.extend(boxed(b"ftyp", b"jxl \0\0\0\0jxl "));
    f.extend_from_slice(rest);
    f
}

#[test]
fn box_sizes_and_offsets() {
    // largesize 接近 u64 上限：位移 + 大小溢位，判為超出檔案
    let mut big = 1u32.to_be_bytes().to_vec();
    big.extend_from_slice(b"jxlc");
    big.extend_from_slice(&(u64::MAX - 4).to_be_bytes());
    let s = scan("largesize.jxl", &container(&big));
    assert_eq!(first(&s).0, "truncated");
    // largesize 小於 16
    let mut small = 1u32.to_be_bytes().to_vec();
    small.extend_from_slice(b"jxlc");
    small.extend_from_slice(&8u64.to_be_bytes());
    assert_eq!(first(&scan("largesize_small.jxl", &container(&small))).1, "box 'jxlc' largesize 8 is too small");
    // 一般大小小於 8
    let mut tiny = 4u32.to_be_bytes().to_vec();
    tiny.extend_from_slice(b"jxlc");
    assert_eq!(first(&scan("size_tiny.jxl", &container(&tiny))).1, "box 'jxlc' size 4 is too small");
    // jxlp 的序號不對、沒有 codestream、沒有 ftyp
    let s = scan("jxlp_index.jxl", &container(&boxed(b"jxlp", &5u32.to_be_bytes())));
    assert_eq!(first(&s).1, "jxlp index 5, expected 0");
    let s = scan("no_codestream.jxl", &container(&boxed(b"xml ", b"<x/>")));
    assert_eq!(first(&s).1, "no codestream box (jxlc or jxlp)");
    let mut f = b"\0\0\0\x0CJXL \r\n\x87\n".to_vec();
    f.extend(boxed(b"jxlc", b"\xff\x0a"));
    assert_eq!(first(&scan("no_ftyp.jxl", &f)).1, "ftyp box missing");
    // Exif 的 TIFF 位移超出 box：只是警告
    let mut exif = u32::MAX.to_be_bytes().to_vec();
    exif.extend_from_slice(b"II*\0");
    let mut rest = boxed(b"Exif", &exif);
    rest.extend(boxed(b"jxlc", b"\xff\x0a"));
    let s = scan("exif_offset.jxl", &container(&rest));
    assert_eq!(s.container_status, "warning");
    assert_eq!(first(&s).1, "Exif TIFF header offset lies outside the box");
}

#[test]
fn broken_codestreams_fail_without_panicking() {
    let view = |name: &str, bytes: &[u8]| {
        let p = write_temp(name, bytes);
        decode_for_view(Path::new(&p), None, &Limits::default(), 1 << 28, &CancelToken::default())
    };
    // 只有簽章、簽章後接垃圾、容器裡的 codestream 只有簽章
    assert!(view("sig_only.jxl", b"\xff\x0a").is_err());
    assert!(view("garbage.jxl", b"\xff\x0a\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff").is_err());
    assert!(view("container_sig.jxl", &container(&boxed(b"jxlc", b"\xff\x0a"))).is_err());
    // 真的測試圖截斷在各個位置
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/generated/s3_jxl_rgba8_srgb.jxl");
    let full = std::fs::read(fixture).expect("fixture");
    for cut in [3, 10, 20, 40, full.len() / 2, full.len() - 1] {
        assert!(view(&format!("cut_{cut}.jxl"), &full[..cut]).is_err(), "cut at {cut}");
    }
    assert!(view("full.jxl", &full).is_ok());
}
