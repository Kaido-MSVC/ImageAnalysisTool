#include "render.hpp"

#include <algorithm>
#include <cstdio>
#include <set>

namespace iat {

namespace {

bool has_issue(const Report& r, std::initializer_list<const char*> codes) {
    for (const Issue& i : r.issues)
        for (const char* c : codes)
            if (i.code == c) return true;
    return false;
}

std::vector<const FileReport*> files(const Report& r) {
    std::vector<const FileReport*> v{&r.original};
    if (r.output) v.push_back(&*r.output);
    return v;
}

std::string mib(uint64_t b) {
    char buf[64];
    if (b >= (1ull << 30)) snprintf(buf, sizeof buf, "%.2f GiB", static_cast<double>(b) / (1ull << 30));
    else snprintf(buf, sizeof buf, "%.2f MiB", static_cast<double>(b) / (1ull << 20));
    return buf;
}

std::string num(uint64_t v) {  // 1,234,567
    std::string s = std::to_string(v), o;
    int k = 0;
    for (auto it = s.rbegin(); it != s.rend(); ++it, ++k) {
        if (k && k % 3 == 0) o.insert(o.begin(), ',');
        o.insert(o.begin(), *it);
    }
    return o;
}

std::string pct(double v) {
    char b[32];
    snprintf(b, sizeof b, "%.2f%%", v * 100.0);
    return b;
}

std::string strict_zh(const std::string& s) {
    if (s == "match") return "**一致**";
    if (s == "mismatch") return "**有差異**";
    if (s == "indeterminate") return "**不可判定**";
    return "**未評估**";
}

std::string decode_zh(const FileReport& f) {
    std::string s;
    if (f.decode_status == "complete") s = "完整";
    else if (f.decode_status == "partial") s = "部分（未完成）";
    else if (f.decode_status == "failed") s = "失敗";
    else if (f.decode_status == "unsupported") s = "不支援";
    else s = "**未檢查**";
    if (!f.decode_status_reason.empty()) s += "（" + md_escape(f.decode_status_reason) + "）";
    return s;
}

std::string container_zh(const std::string& s) {
    if (s == "valid") return "通過";
    if (s == "warning") return "有警告";
    if (s == "corrupt") return "損壞";
    if (s == "over_limit") return "超出限額";
    return "未驗證";
}

std::string samples_zh(const SampleComparison& s) {
    if (s.status == "equal") return "相等（" + num(s.pixels_compared ? s.pixels_compared->v : 0) + " 像素逐值比對）";
    if (s.status == "differs")
        return "不同：" + num(s.pixels_differing ? s.pixels_differing->v : 0) + " / " +
               num(s.pixels_compared ? s.pixels_compared->v : 0) + " 像素";
    if (s.status == "not_comparable") return "不可比較（" + md_escape(s.reason) + "）";
    if (s.status == "incomplete") return "未完成（部分資料不能作為結論）";
    return "未比較（" + md_escape(s.reason) + "）";
}

std::string color_zh(const std::string& s) {
    if (s == "equal") return "一致";
    if (s == "differs") return "不同";
    if (s == "unverified") return "未驗證";
    return "未知";
}

std::string exit_zh(int c) {
    switch (c) {
        case 0: return "0：嚴格一致／檢查成功";
        case 1: return "1：可確認的嚴格差異";
        case 2: return "2：不可判定或超出支援範圍";
        case 3: return "3：解碼或結構失敗";
        case 4: return "4：資源上限、期限或取消（未檢查）";
        default: return "5：參數、I/O、worker 或環境錯誤";
    }
}

std::string row(std::initializer_list<std::string> cells) {
    std::string s = "|";
    for (const auto& c : cells) s += " " + c + " |";
    return s + "\n";
}

std::string join_vals(const std::vector<uint32_t>& v) {
    std::string s;
    for (size_t i = 0; i < v.size(); ++i) s += (i ? ", " : "") + std::to_string(v[i]);
    return s;
}

}  // namespace

std::string md_escape(const std::string& in, size_t max_chars) {
    std::string out;
    size_t chars = 0;
    for (size_t i = 0; i < in.size(); ++i) {
        const unsigned char c = static_cast<unsigned char>(in[i]);
        if ((c & 0xC0) != 0x80) {  // count UTF-8 code points
            if (chars == max_chars) { out += "…"; break; }
            ++chars;
        }
        if (c < 0x20 || c == 0x7F) { out += ' '; continue; }
        // Inline/table-significant characters anywhere; block markers only at the start.
        const bool always = std::string("\\`*_[]<>|~").find(static_cast<char>(c)) != std::string::npos;
        const bool leading = out.empty() && std::string("#+-!=").find(static_cast<char>(c)) != std::string::npos;
        if (always || leading) out += '\\';
        out += static_cast<char>(c);
    }
    return out;
}

ExitDecision decide_exit(const Report& r) {
    using namespace code;
    if (has_issue(r, {input_open_failed, environment_error, worker_crashed, worker_protocol_error, report_write_failed}))
        return {5, "run_error"};
    for (const FileReport* f : files(r))
        if (f->decode_status == "not_checked" &&
            (f->decode_status_reason == "resource_limit" || f->decode_status_reason == "deadline" ||
             f->decode_status_reason == "cancelled"))
            return {4, f->decode_status_reason};
    if (has_issue(r, {cancelled})) return {4, "cancelled"};
    if (has_issue(r, {deadline_exceeded})) return {4, "deadline"};
    if (has_issue(r, {resource_limit})) return {4, "resource_limit"};
    for (const FileReport* f : files(r))
        if (f->container_status == "corrupt" || f->decode_status == "failed" || f->decode_status == "partial")
            return {3, "decode_or_structure_failure"};
    if (r.mode == "compare") {
        if (r.strict.result == "mismatch") return {1, "strict_mismatch"};
        if (r.strict.result == "match") return {0, "strict_match"};
        return {2, r.strict.result};
    }
    // inspect: success only for a complete decode with a clean container and metadata.
    const FileReport& f = r.original;
    if (f.decode_status != "complete") return {2, "unsupported_or_not_checked"};
    if (f.container_status != "valid") return {2, "container_warning"};
    if (f.metadata_status != "valid") return {2, "metadata_" + f.metadata_status};
    return {0, "inspect_complete"};
}

std::string render_markdown(const Report& r) {
    const bool cmp = r.mode == "compare" && r.output.has_value();
    std::string m;
    m += "# 影像分析報告\n\n";
    m += "- 模式：" + std::string(cmp ? "compare（原檔對輸出檔）" : "inspect（單檔）") + "\n";
    m += "- 執行 ID：`" + r.run_id + "`\n";
    auto tv = r.tool.find("version");
    m += "- 工具：iat " + (tv != r.tool.end() ? md_escape(tv->second) : std::string("?"));
    for (const auto& [k, v] : r.codecs) m += "，" + md_escape(k) + " " + md_escape(v);
    m += "\n\n## 結論\n\n";
    m += row({"項目", "結果"}) + row({"---", "---"});
    if (cmp) {
        std::string reasons;
        for (const auto& x : r.strict.reasons) reasons += (reasons.empty() ? "" : "、") + md_escape(x);
        m += row({"嚴格結論", strict_zh(r.strict.result) + (reasons.empty() ? "" : "（" + reasons + "）")});
    }
    m += row({"退出碼", exit_zh(r.exit_code)});
    m += row({cmp ? "原檔解碼" : "解碼", decode_zh(r.original) + "；容器 " + container_zh(r.original.container_status)});
    if (cmp) {
        m += row({"輸出檔解碼", decode_zh(*r.output) + "；容器 " + container_zh(r.output->container_status)});
        m += row({"樣本", samples_zh(r.samples)});
        m += row({"色彩", color_zh(r.color.status) + (r.color.basis.empty() ? "" : "：" + md_escape(r.color.basis))});
        m += row({"ICC", md_escape(r.icc.status)});
        std::string sz = num(r.size.original_bytes.v) + " bytes（" + mib(r.size.original_bytes.v) + "）→ ";
        sz += r.size.output_bytes ? num(r.size.output_bytes->v) + " bytes（" + mib(r.size.output_bytes->v) + "）" : "?";
        if (r.size.output_ratio) sz += "；輸出占比 " + pct(*r.size.output_ratio) + "，節省率 " + pct(*r.size.saving_ratio);
        if (r.size.compression_factor) {
            char b[32];
            snprintf(b, sizeof b, "%.3f", *r.size.compression_factor);
            sz += "，壓縮倍數 " + std::string(b);
        }
        if (!r.size.comparable) sz += "（**不可作為成功轉檔的容量比較**）";
        m += row({"容量", sz});
    } else {
        m += row({"大小", num(r.original.size_bytes.v) + " bytes（" + mib(r.original.size_bytes.v) + "）"});
    }

    // ---- files
    m += "\n## 檔案屬性\n\n";
    std::vector<const FileReport*> fs = files(r);
    if (cmp) m += row({"欄位", "原檔", "輸出檔"}) + row({"---", "---", "---"});
    else m += row({"欄位", "值"}) + row({"---", "---"});
    auto line = [&](const std::string& label, auto get) {
        if (cmp) m += row({label, get(*fs[0]), get(*fs[1])});
        else m += row({label, get(*fs[0])});
    };
    line("檔名", [](const FileReport& f) { return md_escape(f.name); });
    if (!r.original.path.empty()) line("完整路徑", [](const FileReport& f) { return md_escape(f.path, 400); });
    line("大小（bytes）", [](const FileReport& f) { return num(f.size_bytes.v); });
    line("SHA-256", [](const FileReport& f) { return "`" + f.sha256 + "`"; });
    line("格式", [](const FileReport& f) { return md_escape(f.format) + (f.extension_mismatch ? "（副檔名不符）" : ""); });
    line("容器檢查", [](const FileReport& f) { return container_zh(f.container_status); });
    line("解碼", [](const FileReport& f) { return decode_zh(f); });
    line("Metadata 解析", [](const FileReport& f) { return md_escape(f.metadata_status); });
    line("儲存尺寸", [](const FileReport& f) { return f.width ? num(f.width) + " × " + num(f.height) : std::string("?"); });
    line("交錯（IHDR）", [](const FileReport& f) { return f.interlace < 0 ? std::string("?") : (f.interlace ? "Adam7" : "無"); });
    line("樣本模型", [](const FileReport& f) { return md_escape(f.sample_model.model); });
    line("位深（容器／比較）", [](const FileReport& f) {
        return std::to_string(f.sample_model.container_bits) + " / " +
               (f.sample_model.channel_bits.empty() ? std::string("?") : std::to_string(f.sample_model.channel_bits[0])) +
               " (" + md_escape(f.sample_model.compare_type) + ")";
    });
    line("Alpha", [](const FileReport& f) { return md_escape(f.sample_model.alpha); });
    line("無損展開", [](const FileReport& f) { return f.sample_model.expansion.empty() ? std::string("—") : md_escape(f.sample_model.expansion); });
    line("方向", [](const FileReport& f) { return std::to_string(f.orientation.value) + "（" + md_escape(f.orientation.source) + "）"; });
    line("有效色彩", [](const FileReport& f) { return md_escape(f.color.description); });
    line("色彩訊號", [](const FileReport& f) {
        std::string s;
        for (const auto& x : f.color.signals) s += (s.empty() ? "" : "、") + md_escape(x);
        return s.empty() ? std::string("—") : s;
    });
    line("ICC", [](const FileReport& f) {
        std::string s = md_escape(f.icc.status);
        if (f.icc.bytes) s += "，" + num(f.icc.bytes->v) + " bytes，`" + f.icc.sha256.substr(0, 16) + "…`";
        return s;
    });
    if (std::any_of(fs.begin(), fs.end(), [](const FileReport* f) { return f->resource_need.has_value(); }))
        line("資源上限不足", [](const FileReport& f) {
            if (!f.resource_need) return std::string("—");
            const ResourceNeed& n = *f.resource_need;
            return md_escape(n.limit) + "：需要約 " + mib(n.needed_bytes.v) + "，目前可用 " + mib(n.allowed_bytes.v) + "（" +
                   md_escape(n.limit) + "=" + std::to_string(n.current_value) + "）；" +
                   (n.suggested_argument.empty() ? std::string("超過可設定的最大值") : "請加 `" + n.suggested_argument + "` 重跑");
        });
    const bool any_psd = std::any_of(fs.begin(), fs.end(), [](const FileReport* f) { return f->psd.has_value(); });
    if (any_psd) {
        line("PSD 版本／模式", [](const FileReport& f) {
            if (!f.psd) return std::string("—");
            return std::string(f.psd->version == 2 ? "PSB" : "PSD") + "，" + md_escape(f.psd->color_mode) + " " +
                   std::to_string(f.psd->depth) + "-bit，" + std::to_string(f.psd->channels) + " 通道，" + md_escape(f.psd->compression);
        });
        line("PSD 合成影像", [](const FileReport& f) {
            if (!f.psd) return std::string("—");
            const std::string& c = f.psd->composite;
            std::string s = c == "saved"       ? "有（Version Info 標示為真正的合成影像）"
                            : c == "not_saved" ? "**沒有**（未開「最大相容性」，不比較）"
                            : c == "flattened" ? "有（沒有圖層，合成影像即影像本身）"
                                               : "**無法確認**（有圖層但沒有 Version Info 0x0421）";
            if (f.psd->composite_uniform) s += "；**整張單一顏色**";
            return s + "；圖層 " + (f.psd->layers_known ? std::to_string(f.psd->layer_count) : std::string("?"));
        });
    }
    const bool any_avif = std::any_of(fs.begin(), fs.end(), [](const FileReport* f) { return f->avif.has_value(); });
    if (any_avif) {
        line("AVIF 格式", [](const FileReport& f) {
            if (!f.avif) return std::string("—");
            return std::to_string(f.avif->depth) + "-bit " + md_escape(f.avif->yuv_format) + "，CICP " +
                   std::to_string(f.avif->color_primaries) + "/" + std::to_string(f.avif->transfer_characteristics) + "/" +
                   std::to_string(f.avif->matrix_coefficients) + "，" + (f.avif->full_range ? "full" : "limited") + " range" +
                   (f.avif->alpha ? "，含 Alpha" : "");
        });
        line("AVIF 無損（metadata）", [](const FileReport& f) {
            if (!f.avif) return std::string("—");
            if (f.avif->lossless_metadata == "consistent") return std::string("符合無損的必要條件（不代表已證實無損）");
            std::string s;
            for (const auto& w : f.avif->lossless_metadata_reasons) s += (s.empty() ? "" : "、") + md_escape(w);
            return "**不符**（" + s + "）";
        });
        line("AVIF 無損（像素驗證）", [](const FileReport& f) {
            if (!f.avif) return std::string("—");
            if (!f.avif->pixel_verified) return "未驗證（" + md_escape(f.avif->pixel_verified_basis) + "）";
            return *f.avif->pixel_verified ? std::string("**已證實**：與對方的原生樣本逐值相等") : std::string("**否**：樣本不同");
        });
    }
    line("解碼路徑", [](const FileReport& f) { return md_escape(f.decode_path); });
    line("已解列數", [](const FileReport& f) { return num(f.coverage.rows_decoded) + " / " + num(f.coverage.rows_total); });
    line("IEND 後多餘 bytes", [](const FileReport& f) {
        return f.coverage.trailing_bytes ? num(f.coverage.trailing_bytes->v) : std::string("未知（未到達結尾）");
    });

    // ---- differences
    if (cmp && r.samples.status == "differs") {
        m += "\n## 樣本差異\n\n";
        m += "單位：" + md_escape(r.samples.unit) + "；" + (r.samples.includes_alpha ? "含 Alpha" : "不含 Alpha") + "。\n\n";
        m += row({"通道", "不同樣本數", "最大絕對誤差", "MAE", "RMSE"}) + row({"---", "---:", "---:", "---:", "---:"});
        for (const auto& c : r.samples.per_channel) {
            char a[32], b[32];
            snprintf(a, sizeof a, "%.6g", c.mae);
            snprintf(b, sizeof b, "%.6g", c.rmse);
            m += row({c.name, num(c.differing.v), std::to_string(c.max_abs_error), a, b});
        }
        m += "\n前 " + std::to_string(r.samples.first_differences.size()) + " 筆差異（raster 順序）：\n\n";
        m += row({"x", "y", "原檔", "輸出檔"}) + row({"---:", "---:", "---", "---"});
        for (const auto& d : r.samples.first_differences)
            m += row({std::to_string(d.x), std::to_string(d.y), join_vals(d.original), join_vals(d.output)});
    }

    if (cmp && !r.metadata.empty()) {
        m += "\n## Metadata（不影響樣本判定）\n\n";
        m += row({"欄位", "狀態", "原檔", "輸出檔"}) + row({"---", "---", "---", "---"});
        for (const auto& f : r.metadata)
            m += row({md_escape(f.name), md_escape(f.status), f.original.empty() ? "—" : md_escape(f.original, 120),
                      f.output.empty() ? "—" : md_escape(f.output, 120)});
    }

    m += "\n## 效能與資源\n\n";
    m += row({"項目", "值"}) + row({"---", "---"});
    for (const auto& [k, v] : r.metrics.time_ms) {
        char b[32];
        snprintf(b, sizeof b, "%.1f ms", v);
        m += row({"時間 " + md_escape(k), b});
    }
    for (const auto& [k, v] : r.metrics.memory_bytes)
        m += row({"記憶體 " + md_escape(k), v ? num(v->v) + " bytes（" + mib(v->v) + "）" : std::string("未量測")});
    if (r.metrics.io_read_bytes) m += row({"讀取 bytes", num(r.metrics.io_read_bytes->v)});
    if (!r.metrics.timing_note.empty()) m += "\n" + md_escape(r.metrics.timing_note, 400) + "\n";

    if (!r.issues.empty()) {
        m += "\n## 問題與警告\n\n";
        m += row({"代碼", "嚴重度", "範圍", "說明"}) + row({"---", "---", "---", "---"});
        for (const auto& i : r.issues)
            m += row({"`" + i.code + "`", md_escape(i.severity), md_escape(i.scope), md_escape(i.message, 300)});
    }

    m += "\n## 限制\n\n";
    m += "- 樣本相等只證明這一對檔案在本工具支援路徑下的觀察結果。\n";
    m += "- 色彩判定依有效色彩描述；不同 ICC 雜湊不代表外觀必然不同，未知或未驗證時嚴格結論為不可判定。\n";
    m += "- 未完成或未檢查的檔案不產生相等結論，其差異統計僅供參考。\n";
    m += "- Metadata 只記錄雜湊與有限欄位，不輸出文字或 GPS 原值。\n";
    return m;
}

}  // namespace iat
