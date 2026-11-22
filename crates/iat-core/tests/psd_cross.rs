//! PSD／PSB 交叉比對：合成影像用 apeireth-image-psd 編碼合併影像資料，再分別用 iat-core 的解碼器
//! （`decode_for_view`，和分析用的是同一個前端）與 apeireth-image-psd 的 `ImageData::decode` 解出，
//! 逐個樣本比較。範圍是 C++ 版 1.0.0 支援的部分：Raw／RLE；Bitmap、灰階、Indexed、RGB；8／16 位元；
//! PSB 的大尺寸（D-R16）。

use std::path::PathBuf;

use apeireth_image_psd::compression::{PlaneLayout, encode_planes};
use apeireth_image_psd::{Compression, PsdFile, Version};
use iat_core::{CancelToken, DecodedImage, Limits, decode_for_view};

/// 決定性的虛擬亂數（測試圖可重現）。
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u8 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33).to_le_bytes()[0] // 取低 8 位元
    }
}

struct Spec {
    psb: bool,
    mode: u16,
    depth: u16,
    channels: u16,
    width: u32,
    height: u32,
    rle: bool,
}

fn row_bytes(s: &Spec) -> usize {
    (s.width as usize * s.depth as usize).div_ceil(8)
}

/// 最小的 PSD／PSB：沒有影像資源（沒有圖層，合併影像算「已平面化」）、圖層區段長度 0。
fn build(s: &Spec, planar: &[u8], palette: &[u8]) -> Vec<u8> {
    let mut f = Vec::new();
    f.extend_from_slice(b"8BPS");
    f.extend_from_slice(&(if s.psb { 2u16 } else { 1 }).to_be_bytes());
    f.extend_from_slice(&[0; 6]);
    f.extend_from_slice(&s.channels.to_be_bytes());
    f.extend_from_slice(&s.height.to_be_bytes());
    f.extend_from_slice(&s.width.to_be_bytes());
    f.extend_from_slice(&s.depth.to_be_bytes());
    f.extend_from_slice(&s.mode.to_be_bytes());
    f.extend_from_slice(&u32::try_from(palette.len()).expect("palette length fits in u32").to_be_bytes());
    f.extend_from_slice(palette);
    f.extend_from_slice(&0u32.to_be_bytes()); // 影像資源
    if s.psb {
        f.extend_from_slice(&0u64.to_be_bytes());
    } else {
        f.extend_from_slice(&0u32.to_be_bytes());
    }
    let comp = if s.rle { Compression::Rle } else { Compression::Raw };
    let layout = PlaneLayout {
        planes: s.channels as usize,
        width: s.width as usize,
        height: s.height as usize,
        depth: s.depth,
        version: if s.psb { Version::Psb } else { Version::Psd },
    };
    f.extend_from_slice(&comp.as_u16().to_be_bytes());
    f.extend_from_slice(&encode_planes(comp, planar, &layout).expect("apeireth encodes the planes"));
    f
}

fn write_temp(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join("iat-core-psd-cross");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let p = dir.join(name);
    std::fs::write(&p, bytes).expect("write synthetic file");
    p
}

/// apeireth-image-psd 解出的平面資料，轉成 iat-core 的正規樣本（同 C++ 版 psd_side 的展開規則）。
fn reference(bytes: &[u8], s: &Spec) -> Vec<u16> {
    let file = PsdFile::from_bytes(bytes).expect("apeireth parses the file");
    let planar = file.image_data.decode(&file.header).expect("apeireth decodes the merged image");
    let (w, h, rb) = (s.width as usize, s.height as usize, row_bytes(s));
    let plane = h * rb;
    let sample = |c: usize, y: usize, x: usize| -> u16 {
        let row = &planar[c * plane + y * rb..][..rb];
        match s.depth {
            16 => u16::from_be_bytes([row[2 * x], row[2 * x + 1]]),
            8 => u16::from(row[x]),
            _ => u16::from((row[x >> 3] >> (7 - (x & 7))) & 1),
        }
    };
    let palette = &file.color_mode_data;
    let mut out = Vec::new();
    for y in 0..h {
        for x in 0..w {
            match s.mode {
                0 => out.push(if sample(0, y, x) != 0 { 0 } else { 1 }),
                2 => {
                    let i = sample(0, y, x) as usize;
                    out.extend([palette[i], palette[256 + i], palette[512 + i]].map(u16::from));
                }
                1 => out.push(sample(0, y, x)),
                _ => out.extend((0..3).map(|c| sample(c, y, x))),
            }
        }
    }
    out
}

fn check(name: &str, s: Spec) {
    let mut rng = Lcg(u64::from(s.width) * 31 + u64::from(s.height) + u64::from(s.depth) * 7 + u64::from(s.mode));
    let plane_bytes = s.channels as usize * s.height as usize * row_bytes(&s);
    // 一部分重複的位元組，讓 RLE 同時有重複與字面 run
    let planar: Vec<u8> = (0..plane_bytes).map(|i| if i % 7 < 3 { 0x5A } else { rng.next() }).collect();
    let palette: Vec<u8> = if s.mode == 2 { (0..768).map(|_| rng.next()).collect() } else { Vec::new() };
    let bytes = build(&s, &planar, &palette);
    let want = reference(&bytes, &s);
    let path = write_temp(name, &bytes);
    let img: DecodedImage = decode_for_view(&path, None, &Limits::default(), u64::MAX, &CancelToken::new())
        .unwrap_or_else(|e| panic!("{name}: iat-core cannot decode: {e}"));
    std::fs::remove_file(&path).ok();
    assert_eq!((img.width, img.height, img.rows), (s.width, s.height, s.height), "{name}");
    assert!(img.error.is_none(), "{name}: {:?}", img.error);
    assert_eq!(img.samples.len(), want.len(), "{name}: sample count");
    if let Some(i) = img.samples.iter().zip(&want).position(|(a, b)| a != b) {
        panic!("{name}: first difference at sample {i}: iat-core {} vs apeireth {}", img.samples[i], want[i]);
    }
}

fn spec(mode: u16, depth: u16, channels: u16, width: u32, height: u32, rle: bool) -> Spec {
    Spec { psb: false, mode, depth, channels, width, height, rle }
}

#[test]
fn rgb_8_and_16_raw_and_rle() {
    check("rgb8_raw.psd", spec(3, 8, 3, 37, 29, false));
    check("rgb8_rle.psd", spec(3, 8, 3, 37, 29, true));
    check("rgb16_raw.psd", spec(3, 16, 3, 23, 17, false));
    check("rgb16_rle.psd", spec(3, 16, 3, 23, 17, true));
}

#[test]
fn gray_bitmap_indexed() {
    check("gray8_rle.psd", spec(1, 8, 1, 41, 13, true));
    check("gray16_raw.psd", spec(1, 16, 1, 19, 11, false));
    check("bitmap_raw.psd", spec(0, 1, 1, 45, 9, false));
    check("bitmap_rle.psd", spec(0, 1, 1, 45, 9, true));
    check("indexed_rle.psd", spec(2, 8, 1, 33, 21, true));
    check("indexed_raw.psd", spec(2, 8, 1, 33, 21, false));
}

#[test]
fn psb_wider_than_the_psd_limit() {
    // PSD 的上限是 30000：PSB 才放得下；RLE 的列位元組數在 PSB 是 32 位元
    check("psb_rgb8_rle.psb", Spec { psb: true, ..spec(3, 8, 3, 30_017, 3, true) });
    check("psb_rgb16_raw.psb", Spec { psb: true, ..spec(3, 16, 3, 30_001, 2, false) });
    check("psb_gray8_rle.psb", Spec { psb: true, ..spec(1, 8, 1, 2, 40_003, true) });
}
