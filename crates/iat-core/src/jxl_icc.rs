//! libjxl 0.12.0 從色彩列舉值產生的 ICC profile（查表，D-R18）。
//!
//! 資料檔 `data/jxl_icc_table.bin` 由 `tools/libjxl_gen/gen_jxl_icc_table.cpp` 連結 C++ 版建置裡的
//! libjxl 0.12.0（BSD-3-Clause，授權見 `licenses/`）產生：每個非自訂的列舉值組合都編碼一張 1×1
//! 的影像，再用 `JxlDecoderGetColorAsICCProfile(JXL_COLOR_PROFILE_TARGET_ORIGINAL)` 讀回，和 C++ 版
//! 報告裡的 ICC 逐位元組相同。自訂白點、原色或 gamma 查不到。
//!
//! 格式（小端）：`IATJICC1`、u32 筆數，每筆為 5 個 u8（色彩空間、白點、原色、轉換函數、演算意圖，
//! 數值同 libjxl 的 JxlColorEncoding；灰階的原色記為 0）、3 個 0、u32 長度、profile 本身。

/// 嵌入的資料檔。
const TABLE: &[u8] = include_bytes!("../data/jxl_icc_table.bin");

/// 資料檔的 SHA-256（也記在 docs/decisions.md D-R18）。
pub const TABLE_SHA256: &str = "569fabcd015146eefdb783021ed0e4b13c8d4e7e930f54ce91218e53653e0d98";

/// libjxl 列舉值的鍵（數值同 libjxl 的 JxlColorEncoding）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnumKey {
    pub color_space: u8,
    pub white_point: u8,
    pub primaries: u8,
    pub transfer_function: u8,
    pub rendering_intent: u8,
}

/// 查 libjxl 產生的 profile；表裡沒有（自訂值、未知值）時為 None。
pub fn synthesized_icc(key: EnumKey) -> Option<&'static [u8]> {
    let want = [key.color_space, key.white_point, key.primaries, key.transfer_function, key.rendering_intent];
    let mut p = 12usize;
    while let Some(head) = TABLE.get(p..p + 12) {
        let len = u32::from_le_bytes([head[8], head[9], head[10], head[11]]) as usize;
        let body = TABLE.get(p + 12..p + 12 + len)?;
        if head[..5] == want {
            return Some(body);
        }
        p += 12 + len;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::sha256_hex;

    #[test]
    fn embedded_table_matches_the_recorded_hash() {
        assert_eq!(&TABLE[..8], b"IATJICC1");
        assert_eq!(sha256_hex(TABLE), TABLE_SHA256);
    }

    #[test]
    fn known_libjxl_profiles() {
        let k = |cs, wp, pr, tf| EnumKey {
            color_space: cs,
            white_point: wp,
            primaries: pr,
            transfer_function: tf,
            rendering_intent: 1,
        };
        // C++ 版 1.0.0 報告中的 sRGB、灰階 sRGB、BT.2100 PQ、BT.2100 HLG
        for (key, len, sha) in [
            (k(0, 1, 1, 13), 504, "87948cdb39dc5fe2"),
            (k(1, 1, 0, 13), 328, "48d006762d583f6e"),
            (k(0, 1, 9, 16), 4324, "745265d126b3bbfd"),
            (k(0, 1, 9, 18), 4324, "8c50cf64e267fd7d"),
        ] {
            let icc = synthesized_icc(key).expect("profile in the table");
            assert_eq!(icc.len(), len, "{key:?}");
            assert!(sha256_hex(icc).starts_with(sha), "{key:?}");
        }
        assert!(synthesized_icc(k(0, 2, 1, 13)).is_none(), "custom white point is not tabulated");
    }
}
