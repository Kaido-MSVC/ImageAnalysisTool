//! 退出碼判定（對照 C++ 版 `render.cpp` 的 `decide_exit`）。
//! 優先序 5 > 4 > 3 > 1 > 2 > 0；報告保留所有原因。

use crate::report::{Report, code};

pub fn decide_exit(r: &Report) -> (i32, String) {
    if r.has_issue(&[
        code::INPUT_OPEN_FAILED,
        code::ENVIRONMENT_ERROR,
        code::WORKER_CRASHED,
        code::WORKER_PROTOCOL_ERROR,
        code::REPORT_WRITE_FAILED,
    ]) {
        return (5, "run_error".into());
    }
    for f in r.files() {
        if f.decode_status == "not_checked"
            && matches!(f.decode_status_reason.as_str(), "resource_limit" | "deadline" | "cancelled")
        {
            return (4, f.decode_status_reason.clone());
        }
    }
    if r.has_issue(&[code::CANCELLED]) {
        return (4, "cancelled".into());
    }
    if r.has_issue(&[code::DEADLINE_EXCEEDED]) {
        return (4, "deadline".into());
    }
    if r.has_issue(&[code::RESOURCE_LIMIT]) {
        return (4, "resource_limit".into());
    }
    if r.files().any(|f| f.container_status == "corrupt" || f.decode_status == "failed" || f.decode_status == "partial")
    {
        return (3, "decode_or_structure_failure".into());
    }
    if r.mode == "compare" {
        return match r.strict.result.as_str() {
            "mismatch" => (1, "strict_mismatch".into()),
            "match" => (0, "strict_match".into()),
            other => (2, other.into()),
        };
    }
    // inspect：只有完整解碼、容器與 metadata 都乾淨才算成功。
    let f = &r.original;
    if f.decode_status != "complete" {
        return (2, "unsupported_or_not_checked".into());
    }
    if f.container_status != "valid" {
        return (2, "container_warning".into());
    }
    if f.metadata_status != "valid" {
        return (2, format!("metadata_{}", f.metadata_status));
    }
    (0, "inspect_complete".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::FileReport;

    fn inspect(decode: &str, container: &str, meta: &str) -> Report {
        Report {
            mode: "inspect".into(),
            original: FileReport {
                decode_status: decode.into(),
                container_status: container.into(),
                metadata_status: meta.into(),
                ..FileReport::default()
            },
            ..Report::default()
        }
    }

    #[test]
    fn inspect_outcomes() {
        assert_eq!(decide_exit(&inspect("complete", "valid", "valid")), (0, "inspect_complete".into()));
        assert_eq!(decide_exit(&inspect("complete", "warning", "valid")), (2, "container_warning".into()));
        assert_eq!(decide_exit(&inspect("complete", "valid", "parse_failed")), (2, "metadata_parse_failed".into()));
        assert_eq!(
            decide_exit(&inspect("unsupported", "unverified", "unverified")),
            (2, "unsupported_or_not_checked".into())
        );
        assert_eq!(decide_exit(&inspect("failed", "valid", "valid")).0, 3);
    }

    #[test]
    fn priority_run_error_over_everything() {
        let mut r = inspect("failed", "corrupt", "valid");
        r.add_issue(code::CANCELLED, "error", "run", "x");
        assert_eq!(decide_exit(&r).0, 4);
        r.add_issue(code::INPUT_OPEN_FAILED, "error", "original", "x");
        assert_eq!(decide_exit(&r), (5, "run_error".into()));
    }
}
