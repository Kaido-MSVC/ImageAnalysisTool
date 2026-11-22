//! GUI 的使用者設定：目前只有介面語言，存在 `%APPDATA%\iat\gui-settings.json`。
//! 讀不到或格式不對時當作沒有設定（照 Windows 顯示語言）；寫入失敗不影響操作。

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 設定檔內容。語言以 `select_bundled_translation` 的語系名稱保存（`en`、`zh_TW`、`ja`），
/// 不存介面上的索引，日後增減語言時舊設定仍然有效。
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub language: Option<String>,
}

/// 設定檔的位置；沒有 `APPDATA` 時為 None（不保存）。
pub fn settings_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA").filter(|v| !v.is_empty())?;
    Some(Path::new(&appdata).join("iat").join("gui-settings.json"))
}

/// 讀取設定；檔案不存在、讀取失敗或內容無效時回傳預設值。
pub fn load(path: &Path) -> Settings {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// 寫入設定：先寫同資料夾的暫存檔再改名取代舊檔，寫到一半中斷也不會留下壞掉的設定檔。
///
/// # Errors
///
/// 建立資料夾、寫檔或改名失敗時回傳 I/O 錯誤。
pub fn save(path: &Path, settings: &Settings) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| io::Error::other("settings path has no parent folder"))?;
    std::fs::create_dir_all(dir)?;
    let text = serde_json::to_vec_pretty(settings).map_err(io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_bad_files() {
        let dir = std::env::temp_dir().join(format!("iat-gui-settings-test-{}", std::process::id()));
        let path = dir.join("sub").join("gui-settings.json");
        assert_eq!(load(&path), Settings::default(), "missing file");
        let s = Settings { language: Some("ja".into()) };
        save(&path, &s).expect("save into a new folder");
        assert_eq!(load(&path), s);
        save(&path, &Settings { language: Some("zh_TW".into()) }).expect("replace the old file");
        assert_eq!(load(&path).language.as_deref(), Some("zh_TW"));
        std::fs::write(&path, b"{ not json").expect("write a broken file");
        assert_eq!(load(&path), Settings::default(), "broken file");
        std::fs::remove_dir_all(&dir).expect("clean up the test folder");
    }
}
