//! Markdown 報告（只有繁中），對照 C++ 版 `src/model/render.cpp` 的 `render_markdown`。

use crate::report::{FileReport, Report, SampleComparison};

/// 跳脫 Markdown 控制字元並限制長度（以 Unicode 字元計）；控制字元換成空白。
pub fn md_escape(s: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(s.len());
    for (chars, c) in s.chars().enumerate() {
        if chars == max_chars {
            out.push('…');
            break;
        }
        if (c as u32) < 0x20 || c == '\x7f' {
            out.push(' ');
            continue;
        }
        let always = "\\`*_[]<>|~".contains(c);
        let leading = out.is_empty() && "#+-!=".contains(c);
        if always || leading {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn esc(s: &str) -> String {
    md_escape(s, 200)
}

fn mib(b: u64) -> String {
    if b >= 1 << 30 {
        format!("{:.2} GiB", b as f64 / (1u64 << 30) as f64)
    } else {
        format!("{:.2} MiB", b as f64 / (1u64 << 20) as f64)
    }
}

/// 1,234,567
fn num(v: u64) -> String {
    let s = v.to_string();
    let mut o = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            o.push(',');
        }
        o.push(ch);
    }
    o
}

fn pct(v: f64) -> String {
    format!("{:.2}%", v * 100.0)
}

/// printf 的 `%.6g`。
fn g6(v: f64) -> String {
    if v == 0.0 || !v.is_finite() {
        return if v == 0.0 { "0".into() } else { v.to_string() };
    }
    let sci = format!("{v:.5e}"); // 例如 1.23457e-5（已四捨五入到 6 位有效數字）
    let (mant, exp) = sci.split_once('e').expect("exponent form");
    let exp: i32 = exp.parse().expect("integer exponent");
    let trim = |s: String| -> String {
        if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
    };
    if !(-4..6).contains(&exp) {
        let m = trim(mant.to_string());
        format!("{m}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
    } else {
        trim(format!("{v:.*}", (5 - exp) as usize))
    }
}

fn strict_zh(s: &str) -> &'static str {
    match s {
        "match" => "**一致**",
        "mismatch" => "**有差異**",
        "indeterminate" => "**不可判定**",
        _ => "**未評估**",
    }
}

fn decode_zh(f: &FileReport) -> String {
    let mut s = match f.decode_status.as_str() {
        "complete" => "完整",
        "partial" => "部分（未完成）",
        "failed" => "失敗",
        "unsupported" => "不支援",
        _ => "**未檢查**",
    }
    .to_string();
    if !f.decode_status_reason.is_empty() {
        s += &format!("（{}）", esc(&f.decode_status_reason));
    }
    s
}

fn container_zh(s: &str) -> &'static str {
    match s {
        "valid" => "通過",
        "warning" => "有警告",
        "corrupt" => "損壞",
        "over_limit" => "超出限額",
        _ => "未驗證",
    }
}

fn samples_zh(s: &SampleComparison) -> String {
    let c = s.pixels_compared.map_or(0, |c| c.0);
    match s.status.as_str() {
        "equal" => format!("相等（{} 像素逐值比對）", num(c)),
        "differs" => format!("不同：{} / {} 像素", num(s.pixels_differing.map_or(0, |c| c.0)), num(c)),
        "not_comparable" => format!("不可比較（{}）", esc(&s.reason)),
        "incomplete" => "未完成（部分資料不能作為結論）".into(),
        _ => format!("未比較（{}）", esc(&s.reason)),
    }
}

fn color_zh(s: &str) -> &'static str {
    match s {
        "equal" => "一致",
        "differs" => "不同",
        "unverified" => "未驗證",
        _ => "未知",
    }
}

fn exit_zh(c: i32) -> &'static str {
    match c {
        0 => "0：嚴格一致／檢查成功",
        1 => "1：可確認的嚴格差異",
        2 => "2：不可判定或超出支援範圍",
        3 => "3：解碼或結構失敗",
        4 => "4：資源上限、期限或取消（未檢查）",
        _ => "5：參數、I/O、worker 或環境錯誤",
    }
}

fn row<S: AsRef<str>>(cells: &[S]) -> String {
    let mut s = String::from("|");
    for c in cells {
        s.push(' ');
        s.push_str(c.as_ref());
        s.push_str(" |");
    }
    s.push('\n');
    s
}

fn join_vals(v: &[u32]) -> String {
    v.iter().map(u32::to_string).collect::<Vec<_>>().join(", ")
}

fn join_esc(v: &[String]) -> String {
    v.iter().map(|x| esc(x)).collect::<Vec<_>>().join("、")
}

pub fn render_markdown(r: &Report) -> String {
    let cmp = r.mode == "compare" && r.output.is_some();
    let mut m = String::new();
    m += "# 影像分析報告\n\n";
    m += &format!("- 模式：{}\n", if cmp { "compare（原檔對輸出檔）" } else { "inspect（單檔）" });
    m += &format!("- 執行 ID：`{}`\n", r.run_id);
    m += &format!("- 工具：iat {}", r.tool.get("version").map_or("?".to_string(), |v| esc(v)));
    for (k, v) in &r.codecs {
        m += &format!("，{} {}", esc(k), esc(v));
    }
    m += "\n\n## 結論\n\n";
    m += &row(&["項目", "結果"]);
    m += &row(&["---", "---"]);
    if cmp {
        let reasons = join_esc(&r.strict.reasons);
        let extra = if reasons.is_empty() { String::new() } else { format!("（{reasons}）") };
        m += &row(&["嚴格結論".to_string(), format!("{}{extra}", strict_zh(&r.strict.result))]);
    }
    m += &row(&["退出碼", exit_zh(r.exit.code)]);
    let dlabel = if cmp { "原檔解碼" } else { "解碼" };
    m += &row(&[
        dlabel.to_string(),
        format!("{}；容器 {}", decode_zh(&r.original), container_zh(&r.original.container_status)),
    ]);
    if let (true, Some(out)) = (cmp, r.output.as_ref()) {
        m += &row(&[
            "輸出檔解碼".to_string(),
            format!("{}；容器 {}", decode_zh(out), container_zh(&out.container_status)),
        ]);
        m += &row(&["樣本".to_string(), samples_zh(&r.samples)]);
        let basis = if r.color.basis.is_empty() { String::new() } else { format!("：{}", esc(&r.color.basis)) };
        m += &row(&["色彩".to_string(), format!("{}{basis}", color_zh(&r.color.status))]);
        m += &row(&["ICC".to_string(), esc(&r.icc.status)]);
        let ob = r.size.original_bytes.0;
        let mut sz = format!("{} bytes（{}）→ ", num(ob), mib(ob));
        sz += &match r.size.output_bytes {
            Some(o) => format!("{} bytes（{}）", num(o.0), mib(o.0)),
            None => "?".into(),
        };
        if let Some(ratio) = r.size.output_ratio {
            sz += &format!("；輸出占比 {}，節省率 {}", pct(ratio), pct(r.size.saving_ratio.unwrap_or(0.0)));
        }
        if let Some(cf) = r.size.compression_factor {
            sz += &format!("，壓縮倍數 {cf:.3}");
        }
        if !r.size.comparable {
            sz += "（**不可作為成功轉檔的容量比較**）";
        }
        m += &row(&["容量".to_string(), sz]);
    } else {
        let b = r.original.size_bytes.0;
        m += &row(&["大小".to_string(), format!("{} bytes（{}）", num(b), mib(b))]);
    }

    // ---- files
    m += "\n## 檔案屬性\n\n";
    let fs: Vec<&FileReport> = r.files().collect();
    if cmp {
        m += &row(&["欄位", "原檔", "輸出檔"]);
        m += &row(&["---", "---", "---"]);
    } else {
        m += &row(&["欄位", "值"]);
        m += &row(&["---", "---"]);
    }
    let mut line = |label: &str, get: &dyn Fn(&FileReport) -> String| {
        let mut cells = vec![label.to_string(), get(fs[0])];
        if cmp {
            cells.push(get(fs[1]));
        }
        m += &row(&cells);
    };
    line("檔名", &|f| esc(&f.name));
    if !r.original.path.is_empty() {
        line("完整路徑", &|f| md_escape(&f.path, 400));
    }
    line("大小（bytes）", &|f| num(f.size_bytes.0));
    line("SHA-256", &|f| format!("`{}`", f.sha256));
    line("格式", &|f| format!("{}{}", esc(&f.format), if f.extension_mismatch { "（副檔名不符）" } else { "" }));
    line("容器檢查", &|f| container_zh(&f.container_status).into());
    line("解碼", &decode_zh);
    line("Metadata 解析", &|f| esc(&f.metadata_status));
    line("儲存尺寸", &|f| {
        if f.width != 0 { format!("{} × {}", num(f.width.into()), num(f.height.into())) } else { "?".into() }
    });
    line("交錯（IHDR）", &|f| match f.interlace {
        i if i < 0 => "?".into(),
        0 => "無".into(),
        _ => "Adam7".into(),
    });
    line("樣本模型", &|f| esc(&f.sample_model.model));
    line("位深（容器／比較）", &|f| {
        let cb = f.sample_model.channel_bits.first().map_or("?".to_string(), u32::to_string);
        format!("{} / {cb} ({})", f.sample_model.container_bits, esc(&f.sample_model.compare_type))
    });
    line("Alpha", &|f| esc(&f.sample_model.alpha));
    line("無損展開", &|f| {
        if f.sample_model.expansion.is_empty() { "—".into() } else { esc(&f.sample_model.expansion) }
    });
    line("方向", &|f| format!("{}（{}）", f.orientation.value, esc(&f.orientation.source)));
    line("有效色彩", &|f| esc(&f.color.description));
    line("色彩訊號", &|f| if f.color.signals.is_empty() { "—".into() } else { join_esc(&f.color.signals) });
    line("ICC", &|f| {
        let mut s = esc(&f.icc.status);
        if let Some(b) = f.icc.bytes {
            s += &format!("，{} bytes，`{}…`", num(b.0), &f.icc.sha256[..f.icc.sha256.len().min(16)]);
        }
        s
    });
    if fs.iter().any(|f| f.resource_need.is_some()) {
        line("資源上限不足", &|f| match &f.resource_need {
            None => "—".into(),
            Some(n) => format!(
                "{}：需要約 {}，目前可用 {}（{}={}）；{}",
                esc(&n.limit),
                mib(n.needed_bytes.0),
                mib(n.allowed_bytes.0),
                esc(&n.limit),
                n.current_value,
                if n.suggested_argument.is_empty() {
                    "超過可設定的最大值".to_string()
                } else {
                    format!("請加 `{}` 重跑", n.suggested_argument)
                }
            ),
        });
    }
    if fs.iter().any(|f| f.psd.is_some()) {
        line("PSD 版本／模式", &|f| match &f.psd {
            None => "—".into(),
            Some(p) => format!(
                "{}，{} {}-bit，{} 通道，{}",
                if p.version == 2 { "PSB" } else { "PSD" },
                esc(&p.color_mode),
                p.depth,
                p.channels,
                esc(&p.compression)
            ),
        });
        line("PSD 合成影像", &|f| match &f.psd {
            None => "—".into(),
            Some(p) => {
                let mut s = match p.composite.as_str() {
                    "saved" => "有（Version Info 標示為真正的合成影像）",
                    "not_saved" => "**沒有**（未開「最大相容性」，不比較）",
                    "flattened" => "有（沒有圖層，合成影像即影像本身）",
                    _ => "**無法確認**（有圖層但沒有 Version Info 0x0421）",
                }
                .to_string();
                if p.composite_uniform {
                    s += "；**整張單一顏色**";
                }
                let layers = if p.layers_known { p.layer_count.to_string() } else { "?".into() };
                format!("{s}；圖層 {layers}")
            }
        });
    }
    if fs.iter().any(|f| f.avif.is_some()) {
        line("AVIF 格式", &|f| match &f.avif {
            None => "—".into(),
            Some(a) => format!(
                "{}-bit {}，CICP {}/{}/{}，{} range{}",
                a.depth,
                esc(&a.yuv_format),
                a.color_primaries,
                a.transfer_characteristics,
                a.matrix_coefficients,
                if a.full_range { "full" } else { "limited" },
                if a.alpha { "，含 Alpha" } else { "" }
            ),
        });
        line("AVIF 無損（metadata）", &|f| match &f.avif {
            None => "—".into(),
            Some(a) if a.lossless_metadata == "consistent" => "符合無損的必要條件（不代表已證實無損）".into(),
            Some(a) => format!("**不符**（{}）", join_esc(&a.lossless_metadata_reasons)),
        });
        line("AVIF 無損（像素驗證）", &|f| match &f.avif {
            None => "—".into(),
            Some(a) => match a.pixel_verified {
                None => format!("未驗證（{}）", esc(&a.pixel_verified_basis)),
                Some(true) => "**已證實**：與對方的原生樣本逐值相等".into(),
                Some(false) => "**否**：樣本不同".into(),
            },
        });
    }
    line("解碼路徑", &|f| esc(&f.decode_path));
    line("已解列數", &|f| format!("{} / {}", num(f.coverage.rows_decoded), num(f.coverage.rows_total)));
    line("IEND 後多餘 bytes", &|f| match f.coverage.trailing_bytes {
        Some(t) => num(t.0),
        None => "未知（未到達結尾）".into(),
    });

    // ---- differences
    if cmp && r.samples.status == "differs" {
        let s = &r.samples;
        m += "\n## 樣本差異\n\n";
        m += &format!("單位：{}；{}。\n\n", esc(&s.unit), if s.includes_alpha { "含 Alpha" } else { "不含 Alpha" });
        m += &row(&["通道", "不同樣本數", "最大絕對誤差", "MAE", "RMSE"]);
        m += &row(&["---", "---:", "---:", "---:", "---:"]);
        for c in &s.per_channel {
            m += &row(&[c.name.clone(), num(c.differing.0), c.max_abs_error.to_string(), g6(c.mae), g6(c.rmse)]);
        }
        m += &format!("\n前 {} 筆差異（raster 順序）：\n\n", s.first_differences.len());
        m += &row(&["x", "y", "原檔", "輸出檔"]);
        m += &row(&["---:", "---:", "---", "---"]);
        for d in &s.first_differences {
            m += &row(&[d.x.to_string(), d.y.to_string(), join_vals(&d.original), join_vals(&d.output)]);
        }
    }

    if cmp && !r.metadata.fields.is_empty() {
        m += "\n## Metadata（不影響樣本判定）\n\n";
        m += &row(&["欄位", "狀態", "原檔", "輸出檔"]);
        m += &row(&["---", "---", "---", "---"]);
        for f in &r.metadata.fields {
            let v = |x: &str| if x.is_empty() { "—".to_string() } else { md_escape(x, 120) };
            m += &row(&[esc(&f.name), esc(&f.status), v(&f.original), v(&f.output)]);
        }
    }

    m += "\n## 效能與資源\n\n";
    m += &row(&["項目", "值"]);
    m += &row(&["---", "---"]);
    for (k, v) in &r.metrics.time_ms {
        m += &row(&[format!("時間 {}", esc(k)), format!("{v:.1} ms")]);
    }
    for (k, v) in &r.metrics.memory_bytes {
        let val = match v {
            Some(c) => format!("{} bytes（{}）", num(c.0), mib(c.0)),
            None => "未量測".into(),
        };
        m += &row(&[format!("記憶體 {}", esc(k)), val]);
    }
    if let Some(io) = r.metrics.io_read_bytes {
        m += &row(&["讀取 bytes".to_string(), num(io.0)]);
    }
    if !r.metrics.timing_note.is_empty() {
        m += &format!("\n{}\n", md_escape(&r.metrics.timing_note, 400));
    }

    if !r.issues.is_empty() {
        m += "\n## 問題與警告\n\n";
        m += &row(&["代碼", "嚴重度", "範圍", "說明"]);
        m += &row(&["---", "---", "---", "---"]);
        for i in &r.issues {
            m += &row(&[format!("`{}`", i.code), esc(&i.severity), esc(&i.scope), md_escape(&i.message, 300)]);
        }
    }

    m += "\n## 限制\n\n";
    m += "- 樣本相等只證明這一對檔案在本工具支援路徑下的觀察結果。\n";
    m += "- 色彩判定依有效色彩描述；不同 ICC 雜湊不代表外觀必然不同，未知或未驗證時嚴格結論為不可判定。\n";
    m += "- 未完成或未檢查的檔案不產生相等結論，其差異統計僅供參考。\n";
    m += "- Metadata 只記錄雜湊與有限欄位，不輸出文字或 GPS 原值。\n";
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_rules() {
        assert_eq!(md_escape("a|b*c", 200), "a\\|b\\*c");
        assert_eq!(md_escape("#x#", 200), "\\#x#");
        assert_eq!(md_escape("a\tb", 200), "a b");
        assert_eq!(md_escape("中文字串", 2), "中文…");
        assert_eq!(md_escape("ab", 2), "ab");
    }

    #[test]
    fn number_formats() {
        assert_eq!(num(0), "0");
        assert_eq!(num(1234567), "1,234,567");
        assert_eq!(num(100), "100");
        assert_eq!(mib(174), "0.00 MiB");
        assert_eq!(mib(3 << 30), "3.00 GiB");
        assert_eq!(g6(0.0), "0");
        assert_eq!(g6(1.5), "1.5");
        assert_eq!(g6(123456.7), "123457");
        assert_eq!(g6(1234567.0), "1.23457e+06");
        assert_eq!(g6(0.0001234567), "0.000123457");
        assert_eq!(g6(0.00001234567), "1.23457e-05");
        assert_eq!(g6(256.0), "256");
    }
}
