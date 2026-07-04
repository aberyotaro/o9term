//! Mac OS 9 (Platinum) 風の配色定義。
//! ピクセル値は 0x00RRGGBB (softbuffer のバッファ形式)。

// ─── Platinum ウィンドウクローム ───
pub const OUTLINE: u32 = 0x222222; // ウィンドウ最外周の輪郭線
pub const FRAME: u32 = 0xCCCCCC; // フレームの地色
pub const BEVEL_LIGHT: u32 = 0xFFFFFF; // 立体感: 左上ハイライト
pub const BEVEL_DARK: u32 = 0x909090; // 立体感: 右下シャドウ
pub const STRIPE: u32 = 0x999999; // タイトルバーの横縞
pub const STRIPE_LIGHT: u32 = 0xEEEEEE; // 横縞の下のハイライト
pub const SEPARATOR: u32 = 0x555555; // タイトルバー下の区切り線
pub const TITLE_TEXT: u32 = 0x000000;
pub const TITLE_TEXT_INACTIVE: u32 = 0x888888;
pub const BOX_FILL: u32 = 0xDDDDDD; // クローズボックス等の地色
pub const BOX_BORDER: u32 = 0x333333;
pub const TAB_BAR_BG: u32 = 0xC2C2C2; // タブバーの地色
pub const TAB_INACTIVE: u32 = 0xADADAD; // 非アクティブタブ
pub const TAB_BORDER: u32 = 0x444444;
pub const DROP_HINT: u32 = 0x555555; // タブドラッグ中のドロップ先プレビュー
pub const SUNKEN_DARK: u32 = 0x808080; // コンテンツ領域の窪み(左上)
pub const SUNKEN_LIGHT: u32 = 0xFFFFFF; // コンテンツ領域の窪み(右下)

// ─── 端末コンテンツ ───
// クラシックMacらしく白地に黒文字をデフォルトにする
pub const CONTENT_BG: u32 = 0xFFFFFF;
pub const CONTENT_FG: u32 = 0x000000;
pub const CURSOR: u32 = 0x000000;

/// ANSI 16色。白背景でも読めるよう Terminal.app のクラシック値に寄せている。
pub const ANSI16: [u32; 16] = [
    0x000000, // black
    0xCC0000, // red
    0x00A600, // green
    0x999900, // yellow
    0x0000B2, // blue
    0xB200B2, // magenta
    0x00A6B2, // cyan
    0xBFBFBF, // white
    0x666666, // bright black
    0xE50000, // bright red
    0x00D900, // bright green
    0xE5E500, // bright yellow
    0x0000FF, // bright blue
    0xE500E5, // bright magenta
    0x00E5E5, // bright cyan
    0xE5E5E5, // bright white
];

/// 端末のカラースキーマ。ウィンドウクロームの配色は Platinum 固定で、
/// これはコンテンツ (文字グリッド) のみに適用される。
pub struct Scheme {
    pub id: &'static str,
    pub name: &'static str,
    pub fg: u32,
    pub bg: u32,
    pub cursor: u32,
    pub ansi: [u32; 16],
}

const SOLARIZED_ANSI: [u32; 16] = [
    0x073642, 0xDC322F, 0x859900, 0xB58900, 0x268BD2, 0xD33682, 0x2AA198, 0xEEE8D5,
    0x002B36, 0xCB4B16, 0x586E75, 0x657B83, 0x839496, 0x6C71C4, 0x93A1A1, 0xFDF6E3,
];

const DARK_ANSI: [u32; 16] = [
    0x000000, 0xCD3131, 0x0DBC79, 0xE5E510, 0x2472C8, 0xBC3FBC, 0x11A8CD, 0xE5E5E5,
    0x666666, 0xF14C4C, 0x23D18B, 0xF5F543, 0x3B8EEA, 0xD670D6, 0x29B8DB, 0xFFFFFF,
];

pub static SCHEMES: &[Scheme] = &[
    Scheme {
        id: "classic",
        name: "クラシック (白地)",
        fg: CONTENT_FG,
        bg: CONTENT_BG,
        cursor: CURSOR,
        ansi: ANSI16,
    },
    Scheme {
        id: "dark",
        name: "ダーク",
        fg: 0xCCCCCC,
        bg: 0x1E1E1E,
        cursor: 0xCCCCCC,
        ansi: DARK_ANSI,
    },
    Scheme {
        id: "solarized-dark",
        name: "Solarized ダーク",
        fg: 0x839496,
        bg: 0x002B36,
        cursor: 0x93A1A1,
        ansi: SOLARIZED_ANSI,
    },
    Scheme {
        id: "solarized-light",
        name: "Solarized ライト",
        fg: 0x657B83,
        bg: 0xFDF6E3,
        cursor: 0x586E75,
        ansi: SOLARIZED_ANSI,
    },
    Scheme {
        id: "green-crt",
        name: "グリーンCRT",
        fg: 0x33FF33,
        bg: 0x0A0A0A,
        cursor: 0x33FF33,
        ansi: DARK_ANSI,
    },
];

/// id からスキーマのインデックスを引く。見つからなければ 0 (クラシック)。
pub fn scheme_index(id: &str) -> usize {
    SCHEMES.iter().position(|s| s.id == id).unwrap_or(0)
}

/// xterm 256色パレットの既定値を返す。
pub fn indexed256(i: u8) -> u32 {
    let i = i as u32;
    if i < 16 {
        return ANSI16[i as usize];
    }
    if i < 232 {
        // 6x6x6 カラーキューブ
        let i = i - 16;
        let levels = [0u32, 95, 135, 175, 215, 255];
        let r = levels[(i / 36) as usize];
        let g = levels[((i / 6) % 6) as usize];
        let b = levels[(i % 6) as usize];
        return (r << 16) | (g << 8) | b;
    }
    // グレースケール
    let v = 8 + (i - 232) * 10;
    (v << 16) | (v << 8) | v
}
