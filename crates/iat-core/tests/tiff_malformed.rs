//! 畸形 TIFF 的大小與位移：掃描與解碼都必須回報錯誤或不支援，不可以 panic 或溢位（D-R17）。

use std::path::{Path, PathBuf};

use iat_core::input::Input;
use iat_core::tiff_scan::scan_tiff;
use iat_core::{CancelToken, Limits, decode_for_view};

fn write_temp(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join("iat-core-tiff-malformed");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let p = dir.join(name);
    std::fs::write(&p, bytes).expect("write synthetic file");
    p
}

/// classic TIFF（小端），第一個 IFD 在位移 8；項目為 (tag, type, count, 4 bytes 的值欄位)，之後接 tail。
fn classic(entries: &[(u16, u16, u32, u32)], tail: &[u8]) -> Vec<u8> {
    let mut f = b"II*\0".to_vec();
    f.extend_from_slice(&8u32.to_le_bytes());
    f.extend_from_slice(&u16::try_from(entries.len()).expect("entry count fits in u16").to_le_bytes());
    for &(tag, typ, count, value) in entries {
        f.extend_from_slice(&tag.to_le_bytes());
        f.extend_from_slice(&typ.to_le_bytes());
        f.extend_from_slice(&count.to_le_bytes());
        f.extend_from_slice(&value.to_le_bytes());
    }
    f.extend_from_slice(&0u32.to_le_bytes());
    f.extend_from_slice(tail);
    f
}

/// BigTIFF（小端），第一個 IFD 在位移 16；項目為 (tag, type, count, 8 bytes 的值欄位)。
fn bigtiff(entries: &[(u16, u16, u64, u64)], tail: &[u8]) -> Vec<u8> {
    let mut f = b"II+\0".to_vec();
    f.extend_from_slice(&8u16.to_le_bytes());
    f.extend_from_slice(&0u16.to_le_bytes());
    f.extend_from_slice(&16u64.to_le_bytes());
    f.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    for &(tag, typ, count, value) in entries {
        f.extend_from_slice(&tag.to_le_bytes());
        f.extend_from_slice(&typ.to_le_bytes());
        f.extend_from_slice(&count.to_le_bytes());
        f.extend_from_slice(&value.to_le_bytes());
    }
    f.extend_from_slice(&0u64.to_le_bytes());
    f.extend_from_slice(tail);
    f
}

fn scan(name: &str, bytes: &[u8]) -> iat_core::tiff_scan::TiffScan {
    let p = write_temp(name, bytes);
    let f = std::fs::File::open(&p).expect("open");
    let input = Input::new(&f).expect("input");
    scan_tiff(&input, &Limits::default())
}

fn first_issue(s: &iat_core::tiff_scan::TiffScan) -> (&str, &str) {
    let i = s.issues.first().expect("an issue");
    (i.code.as_str(), i.message.as_str())
}

/// 一張 1×1 的 8 位元灰階影像的基本項目；extra 取代同編號的 tag 或新增。
fn gray(extra: &[(u16, u16, u32, u32)], data_at: u32) -> Vec<(u16, u16, u32, u32)> {
    let mut e = vec![
        (256, 4, 1, 1),
        (257, 4, 1, 1),
        (258, 3, 1, 8),
        (262, 3, 1, 1),
        (273, 4, 1, data_at),
        (277, 3, 1, 1),
        (279, 4, 1, 1),
    ];
    e.retain(|x| !extra.iter().any(|y| y.0 == x.0));
    e.extend_from_slice(extra);
    e.sort_by_key(|x| x.0);
    e
}

#[test]
fn header_and_ifd_offsets() {
    let s = scan("short.tif", b"II*\0");
    assert_eq!(first_issue(&s), ("truncated", "file shorter than a TIFF header"));
    let s = scan("bom.tif", b"XX*\0\x08\0\0\0");
    assert_eq!(first_issue(&s).1, "bad byte-order mark");
    let s = scan("magic.tif", b"II\x2b\0\x04\0\0\0\0\0\0\0\0\0\0\0");
    assert_eq!(first_issue(&s).1, "BigTIFF offset size must be 8");
    let mut f = b"II*\0".to_vec();
    f.extend_from_slice(&u32::MAX.to_le_bytes());
    let s = scan("ifd_far.tif", &f);
    assert_eq!(first_issue(&s).1, "first IFD offset 4294967295 is outside the file");
    // BigTIFF：IFD 位移接近 u64 上限，加上計數欄位會溢位
    let mut f = b"II+\0\x08\0\0\0".to_vec();
    f.extend_from_slice(&(u64::MAX - 3).to_le_bytes());
    let s = scan("big_ifd_far.tif", &f);
    assert_eq!(s.container_status, "corrupt");
    // 項目數 0 與太多
    let s = scan("ifd_zero.tif", &classic(&[], &[]));
    assert_eq!(first_issue(&s).1, "IFD entry count 0 is not plausible");
    let mut f = b"II*\0\x08\0\0\0".to_vec();
    f.extend_from_slice(&u16::MAX.to_le_bytes());
    let s = scan("ifd_many.tif", &f);
    assert_eq!(first_issue(&s).1, "IFD entry count 65535 is not plausible");
}

#[test]
fn tag_sizes_and_offsets_overflow() {
    // BigTIFF：count × 型別大小溢位
    let s = scan("count_overflow.tif", &bigtiff(&[(256, 16, u64::MAX / 4, 0)], &[]));
    assert_eq!(first_issue(&s).1, "tag 256 value size overflows");
    // BigTIFF：值的位移 + 大小溢位
    let s = scan("value_overflow.tif", &bigtiff(&[(270, 2, 100, u64::MAX - 10)], &[]));
    assert_eq!(first_issue(&s).1, "tag 270 value lies outside the file");
    // strip 的位移 + byte count 溢位：起點在檔外，判為結構錯誤
    let s = scan(
        "strip_overflow.tif",
        &bigtiff(
            &[
                (256, 4, 1, 1),
                (257, 4, 1, 1),
                (258, 3, 1, 8),
                (262, 3, 1, 1),
                (273, 16, 1, u64::MAX - 1),
                (277, 3, 1, 1),
                (279, 16, 1, 10),
            ],
            &[],
        ),
    );
    assert_eq!(first_issue(&s).0, "structure_error");
    assert!(first_issue(&s).1.starts_with("block 0 at offset 18446744073709551614 (+10 bytes) lies outside the file"));
    // 寬高最大、一條 strip：容器本身合法（解碼端另外拒絕，見下一個測試）
    let s = scan("huge_geometry.tif", &classic(&gray(&[(256, 4, 1, u32::MAX), (257, 4, 1, u32::MAX)], 0), &[]));
    assert_eq!((s.container_status, s.width, s.height), ("valid", u32::MAX, u32::MAX));
    // 寬高最大、每列一條 strip：表只有一筆，個數不符
    let s = scan(
        "huge_strips.tif",
        &classic(&gray(&[(256, 4, 1, u32::MAX), (257, 4, 1, u32::MAX), (278, 4, 1, 1)], 0), &[]),
    );
    assert_eq!(first_issue(&s).1, "IFD lists 1 offsets and 1 byte counts, geometry needs 4294967295");
    // 分平面：expected × spp 在 C++ 版是迴繞的 64 位元乘法，這裡也不可 panic
    let s = scan(
        "planar_wrap.tif",
        &classic(&gray(&[(277, 3, 1, 65535), (278, 4, 1, 1), (284, 3, 1, 2), (257, 4, 1, u32::MAX)], 0), &[]),
    );
    assert_eq!(s.container_status, "corrupt");
    // tile 尺寸必須是 16 的倍數
    let s = scan(
        "tile_odd.tif",
        &classic(&gray(&[(322, 4, 1, 15), (323, 4, 1, 16), (324, 4, 1, 0), (325, 4, 1, 1)], 0), &[]),
    );
    assert_eq!(first_issue(&s).1, "tile size 15x16 is invalid (must be a non-zero multiple of 16)");
}

fn view(name: &str, bytes: &[u8]) -> Result<iat_core::DecodedImage, iat_core::ViewError> {
    let p = write_temp(name, bytes);
    decode_for_view(Path::new(&p), None, &Limits::default(), 1 << 28, &CancelToken::default())
}

#[test]
fn decoder_rejects_oversized_blocks_without_panicking() {
    // 2^31 × 2^31 的 tile：tile 大小超過 64 位元，回報資源上限而不是溢位
    let tile = 1u32 << 31;
    let f = classic(
        &gray(&[(322, 4, 1, tile), (323, 4, 1, tile), (324, 4, 1, 0), (325, 4, 1, 1)], 0)
            .into_iter()
            .filter(|e| e.0 != 273 && e.0 != 279)
            .collect::<Vec<_>>(),
        &[],
    );
    let p = write_temp("tile_huge.tif", &f);
    let s = {
        let file = std::fs::File::open(&p).expect("open");
        scan_tiff(&Input::new(&file).expect("input"), &Limits::default())
    };
    assert_eq!(s.container_status, "valid", "{:?}", s.issues);
    assert!(view("tile_huge.tif", &f).is_err());
    // 寬度 2^32−1 的單列 strip：預算擋下，不會配置
    let tail = [0u8; 16];
    let f = classic(&gray(&[(256, 4, 1, u32::MAX)], 8 + 2 + 7 * 12 + 4), &tail);
    assert!(view("wide.tif", &f).is_err());
    // 寬高最大的單一 strip：在配置任何緩衝之前就被檢視上限擋下
    let f = classic(&gray(&[(256, 4, 1, u32::MAX), (257, 4, 1, u32::MAX)], 0), &[]);
    assert!(matches!(view("huge.tif", &f), Err(iat_core::ViewError::OverViewLimit { .. })));
    // 1×1、資料正常：可以解
    let f = classic(&gray(&[], 8 + 2 + 7 * 12 + 4), &[0x2a]);
    let img = view("ok.tif", &f).expect("1x1 gray decodes");
    assert_eq!(img.samples, [0x2a]);
    // PackBits 資料不足：失敗但不 panic
    let f = classic(&gray(&[(259, 3, 1, 32773)], 8 + 2 + 8 * 12 + 4), &[5]);
    assert!(view("packbits_short.tif", &f).is_err());
}
