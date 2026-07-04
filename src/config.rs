//! 設定の永続化。~/.config/os9terminal/config.toml に保存する。
//! 項目が少ないため TOML は手書きで読み書きする (依存を増やさない)。

use std::path::PathBuf;

fn config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config/os9terminal/config.toml"))
}

/// 保存されたカラースキーマの id を返す。
pub fn load_scheme() -> Option<String> {
    let content = std::fs::read_to_string(config_path()?).ok()?;
    for line in content.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("scheme") else { continue };
        let Some(value) = rest.trim_start().strip_prefix('=') else { continue };
        return Some(value.trim().trim_matches('"').to_string());
    }
    None
}

pub fn save_scheme(id: &str) {
    let Some(path) = config_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, format!("scheme = \"{id}\"\n"));
}
