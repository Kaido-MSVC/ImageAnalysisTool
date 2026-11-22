//! 從報告整理出結果畫面要用的資訊。只影響 GUI 的呈現；報告、JSON 與退出碼都不動。

use iat_core::Report;
use iat_core::report::{EffectiveColor, FileReport};

/// 結果中有 `resource_limit`，而且某一側附有 `job_commit_mib` 的建議值時，回傳建議值（MiB，兩側取大）。
/// 只讀報告的結構化欄位 `resource_need`，不解析訊息文字。
pub fn suggested_job_commit_mib(r: &Report) -> Option<u64> {
    let limited = r.exit.reason == "resource_limit" || r.issues.iter().any(|i| i.code == "resource_limit");
    if !limited {
        return None;
    }
    std::iter::once(&r.original)
        .chain(r.output.as_ref())
        .filter_map(|f| f.resource_need.as_ref())
        .filter(|n| n.limit == "job_commit_mib")
        .filter_map(|n| n.suggested_value)
        .max()
}

/// 「像素一致」：compare 的 strict 為無法判定、樣本相等，而且原因只有 `color_unverified`。
pub fn pixel_identical(r: &Report) -> bool {
    r.mode == "compare"
        && r.strict.result == "indeterminate"
        && r.samples.status == "equal"
        && r.strict.reasons.len() == 1
        && r.strict.reasons[0] == "color_unverified"
}

/// JXL 色彩列舉值 key（`jxl_enum:space=0;white=1;primaries=1;transfer=13`）的色彩空間名稱。
/// 數值是 libjxl 的 JxlColorSpace／JxlWhitePoint／JxlPrimaries／JxlTransferFunction。
pub fn jxl_enum_name(key: &str) -> Option<String> {
    let body = key.strip_prefix("jxl_enum:")?;
    let field = |name: &str| -> Option<i64> {
        body.split(';').find_map(|kv| kv.strip_prefix(name)?.strip_prefix('=')?.parse().ok())
    };
    let (space, white, prim, tf) = (field("space")?, field("white")?, field("primaries"), field("transfer")?);
    let tf_name = match tf {
        1 => "BT.709",
        8 => "linear",
        13 => "sRGB",
        16 => "PQ",
        17 => "DCI",
        18 => "HLG",
        _ => "custom transfer",
    };
    let white_name = match white {
        1 => "D65",
        10 => "E",
        11 => "DCI",
        _ => "custom white point",
    };
    if space == 1 {
        return Some(match (white, tf) {
            (1, 13) => "Gray (sRGB transfer)".into(),
            (1, _) => format!("Gray ({tf_name})"),
            _ => format!("Gray ({white_name}, {tf_name})"),
        });
    }
    if space != 0 {
        return None;
    }
    let name = match (white, prim?, tf) {
        (1, 1, 13) => "sRGB".to_string(),
        (1, 1, 8) => "Linear sRGB".into(),
        (1, 1, 1) => "Rec. 709".into(),
        (1, 11, 13) => "Display P3".into(),
        (1, 11, 8) => "Linear Display P3".into(),
        (11, 11, 17) => "DCI-P3".into(),
        (1, 9, 16) => "Rec. 2100 PQ".into(),
        (1, 9, 18) => "Rec. 2100 HLG".into(),
        (1, 9, 1) => "Rec. 2020".into(),
        (1, 9, 8) => "Linear Rec. 2020".into(),
        (_, p, _) => {
            let prim_name = match p {
                1 => "sRGB primaries",
                9 => "BT.2100 primaries",
                11 => "P3 primaries",
                _ => "custom primaries",
            };
            format!("RGB, {white_name}, {prim_name}, {tf_name}")
        }
    };
    Some(name)
}

/// 色彩欄的依據：`jxl_enum` 後面加上實際的色彩空間名稱，例如 `jxl_enum (sRGB)`。
/// 依據文字的順序是原檔在前、輸出在後（`<原檔> vs <輸出>`），所以依出現順序替換。
pub fn color_basis_text(r: &Report) -> String {
    let basis = r.color.basis.clone();
    let names: Vec<Option<String>> = std::iter::once(&r.original)
        .chain(r.output.as_ref())
        .map(|f| if f.color.kind == "jxl_enum" { jxl_enum_name(&f.color.key) } else { None })
        .collect();
    let named: Vec<&String> = names.iter().flatten().collect();
    // 兩側都是 jxl_enum 時依出現順序對應；只有一側時每個出現的位置都是那一側。
    let single = match named.as_slice() {
        [] => return basis,
        [one] => Some(*one),
        _ => None,
    };
    let mut in_order = named.iter();
    let mut out = String::new();
    let mut rest = basis.as_str();
    const TOKEN: &str = "jxl_enum";
    while let Some(pos) = rest.find(TOKEN) {
        out.push_str(&rest[..pos + TOKEN.len()]);
        rest = &rest[pos + TOKEN.len()..];
        if let Some(n) = single.or_else(|| in_order.next().copied()) {
            out.push_str(&format!(" ({n})"));
        }
    }
    out.push_str(rest);
    out
}

/// 結果畫面的白話說明用：色彩來源的種類（`EffectiveColor::kind`）與細節。
/// 細節依種類而定：JXL 為色彩空間名稱、cICP 為四個數值、gAMA／cHRM 為實際有的 chunk（以 / 分隔）。
pub fn color_source(f: &FileReport) -> (String, String) {
    let c: &EffectiveColor = &f.color;
    let detail = match c.kind.as_str() {
        "jxl_enum" => jxl_enum_name(&c.key).unwrap_or_default(),
        "cicp" => c.key.strip_prefix("cicp:").unwrap_or_default().to_string(),
        "partial" | "chrm_gama" => {
            let chunks: Vec<&str> = ["gAMA", "cHRM"]
                .into_iter()
                .filter(|n| c.signals.iter().any(|s| s.starts_with(&format!("{n}("))))
                .collect();
            chunks.join("/")
        }
        _ => String::new(),
    };
    (c.kind.clone(), detail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iat_core::report::{Count, Issue, ResourceNeed};

    fn need(limit: &str, v: Option<u64>) -> ResourceNeed {
        ResourceNeed {
            limit: limit.into(),
            needed_bytes: Count(1),
            allowed_bytes: Count(1),
            current_value: 1024,
            suggested_value: v,
            suggested_argument: String::new(),
        }
    }

    fn color(kind: &str, key: &str, signals: &[&str]) -> EffectiveColor {
        EffectiveColor {
            kind: kind.into(),
            key: key.into(),
            description: String::new(),
            signals: signals.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn compare_report() -> Report {
        Report { mode: "compare".into(), output: Some(FileReport::default()), ..Report::default() }
    }

    #[test]
    fn suggestion_needs_resource_limit_and_job_commit() {
        let mut r = compare_report();
        r.output.as_mut().expect("compare has output").resource_need = Some(need("job_commit_mib", Some(1536)));
        assert_eq!(suggested_job_commit_mib(&r), None, "no resource_limit in the result");
        r.issues.push(Issue::new("resource_limit", "error", "output", "x"));
        assert_eq!(suggested_job_commit_mib(&r), Some(1536));
        r.original.resource_need = Some(need("job_commit_mib", Some(2048)));
        assert_eq!(suggested_job_commit_mib(&r), Some(2048), "larger of the two sides");
        r.original.resource_need = Some(need("buffer_mib", Some(4096)));
        assert_eq!(suggested_job_commit_mib(&r), Some(1536), "other limits are not offered");
        r.output.as_mut().expect("compare has output").resource_need = Some(need("job_commit_mib", None));
        assert_eq!(suggested_job_commit_mib(&r), None, "need beyond the largest value");
    }

    #[test]
    fn pixel_identical_only_for_color_unverified() {
        let mut r = compare_report();
        r.strict.result = "indeterminate".into();
        r.samples.status = "equal".into();
        r.strict.reasons = vec!["color_unverified".into()];
        assert!(pixel_identical(&r));
        r.strict.reasons.push("trailing_data".into());
        assert!(!pixel_identical(&r), "another reason");
        r.strict.reasons = vec!["color_unknown".into()];
        assert!(!pixel_identical(&r), "other indeterminate reason");
        r.strict.reasons = vec!["color_unverified".into()];
        r.samples.status = "differs".into();
        assert!(!pixel_identical(&r), "samples differ");
    }

    #[test]
    fn jxl_names() {
        assert_eq!(jxl_enum_name("jxl_enum:space=0;white=1;primaries=1;transfer=13").as_deref(), Some("sRGB"));
        assert_eq!(jxl_enum_name("jxl_enum:space=0;white=1;primaries=9;transfer=16").as_deref(), Some("Rec. 2100 PQ"));
        assert_eq!(jxl_enum_name("jxl_enum:space=1;white=1;transfer=13").as_deref(), Some("Gray (sRGB transfer)"));
        assert_eq!(
            jxl_enum_name("jxl_enum:space=0;white=10;primaries=11;transfer=8").as_deref(),
            Some("RGB, E, P3 primaries, linear")
        );
        assert_eq!(jxl_enum_name("icc:abc"), None);
    }

    #[test]
    fn basis_names_jxl_sides_in_order() {
        let mut r = compare_report();
        r.original.color = color("none", "", &[]);
        r.output.as_mut().expect("compare has output").color =
            color("jxl_enum", "jxl_enum:space=0;white=1;primaries=1;transfer=13", &[]);
        r.color.basis = "none vs jxl_enum (no reliable equivalence)".into();
        assert_eq!(color_basis_text(&r), "none vs jxl_enum (sRGB) (no reliable equivalence)");
        r.original.color = color("jxl_enum", "jxl_enum:space=0;white=1;primaries=9;transfer=16", &[]);
        r.color.basis = "jxl_enum vs jxl_enum (no reliable equivalence)".into();
        assert_eq!(color_basis_text(&r), "jxl_enum (Rec. 2100 PQ) vs jxl_enum (sRGB) (no reliable equivalence)");
        r.color.basis = "neither file declares colour information".into();
        assert_eq!(color_basis_text(&r), "neither file declares colour information");
    }

    #[test]
    fn color_source_details() {
        let mut f = FileReport {
            color: color("partial", "", &["cHRM(31270,32900,64000,33000,30000,60000,15000,6000)"]),
            ..FileReport::default()
        };
        assert_eq!(color_source(&f), ("partial".into(), "cHRM".into()));
        f.color = color("chrm_gama", "gama:45455;chrm:1", &["gAMA(45455)", "cHRM(1)"]);
        assert_eq!(color_source(&f).1, "gAMA/cHRM");
        f.color = color("cicp", "cicp:1/13/0/1", &[]);
        assert_eq!(color_source(&f).1, "1/13/0/1");
        f.color = color("webp_default_srgb", "webp_default_srgb", &[]);
        assert_eq!(color_source(&f), ("webp_default_srgb".into(), String::new()));
    }
}
