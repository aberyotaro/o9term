//! 設定の永続化。~/.config/os9terminal/config.toml に保存する。
//! 項目が少ないため TOML は手書きで読み書きする (依存を増やさない)。

use std::path::PathBuf;

fn config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config/os9terminal/config.toml"))
}

/// `key = ...` 形式の行なら、`=` より後ろをそのまま返す。
fn line_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let (name, value) = line.split_once('=')?;
    (name.trim() == key).then_some(value)
}

fn value<'a>(content: &'a str, key: &str) -> Option<&'a str> {
    content.lines().find_map(|line| {
        line_value(line, key).map(|value| value.split('#').next().unwrap_or("").trim())
    })
}

/// 他の設定やコメントを保ったまま、指定したキーだけを書き換える。
fn update(content: &str, key: &str, value: &str) -> String {
    let mut found = false;
    let mut lines = Vec::new();
    for line in content.lines() {
        if line_value(line, key).is_some() {
            if !found {
                lines.push(format!("{key} = {value}"));
                found = true;
            }
        } else {
            lines.push(line.to_owned());
        }
    }
    if !found {
        lines.push(format!("{key} = {value}"));
    }
    lines.join("\n") + "\n"
}

fn load() -> Option<String> {
    std::fs::read_to_string(config_path()?).ok()
}

fn save(key: &str, value: &str) {
    let Some(path) = config_path() else { return };
    if let Some(dir) = path.parent()
        && std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let _ = std::fs::write(path, update(&content, key, value));
}

/// 保存されたカラースキーマの id を返す。
pub fn load_scheme() -> Option<String> {
    Some(value(&load()?, "scheme")?.trim_matches('"').to_owned())
}

pub fn save_scheme(id: &str) {
    save("scheme", &format!("\"{id}\""));
}

pub fn load_bitmap_text() -> bool {
    load().is_some_and(|content| value(&content, "bitmap_text") == Some("true"))
}

pub fn save_bitmap_text(enabled: bool) {
    save("bitmap_text", if enabled { "true" } else { "false" });
}

#[cfg(test)]
mod tests {
    use super::{update, value};

    #[test]
    fn changing_one_setting_preserves_other_settings_and_comments() {
        let original = "# Personal settings\nscheme = \"dark\"\nfuture = 42\n";
        let bitmap = update(original, "bitmap_text", "true");
        let changed = update(&bitmap, "scheme", "\"classic\"");
        assert_eq!(value(&changed, "bitmap_text"), Some("true"));
        assert_eq!(value(&changed, "scheme"), Some("\"classic\""));
        assert!(changed.contains("# Personal settings\n"));
        assert!(changed.contains("future = 42\n"));
    }

    #[test]
    fn matching_keys_are_exact_and_comments_are_ignored() {
        let content = "scheme_backup = \"dark\"\n# scheme = \"dark\"\n bitmap_text = true # crisp\n";
        assert_eq!(value(content, "scheme"), None);
        assert_eq!(value(content, "bitmap_text"), Some("true"));
        let result = update(content, "scheme", "\"classic\"");
        assert!(result.contains("scheme_backup = \"dark\""));
        assert_eq!(value(&result, "scheme"), Some("\"classic\""));
    }

    #[test]
    fn update_replaces_in_place_collapses_duplicates_and_appends_when_missing() {
        let content = "a = 1\nscheme = \"dark\"\n# scheme = x\nb = 2\nscheme=\"old\"";
        assert_eq!(
            update(content, "scheme", "\"new\""),
            "a = 1\nscheme = \"new\"\n# scheme = x\nb = 2\n",
        );
        assert_eq!(update("a = 1", "b", "2"), "a = 1\nb = 2\n");
        assert_eq!(update("", "b", "2"), "b = 2\n");
    }

    #[test]
    fn value_uses_the_first_match_and_splits_at_the_first_equals_sign() {
        assert_eq!(value("k = 1\nk = 2\n", "k"), Some("1"));
        assert_eq!(value("k = a = b # c\n", "k"), Some("a = b"));
        assert_eq!(value("k =\n", "k"), Some(""));
        assert_eq!(value("k\n", "k"), None);
    }
}
