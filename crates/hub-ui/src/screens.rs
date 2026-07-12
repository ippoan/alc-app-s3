//! 各画面の描画。基準レイアウトは 320x240 横向き (上部 18px はステータスバー)。
//!
//! - 基準文字サイズは「2 倍」: u8g2 の日本語フォントは 16px が最大のため、
//!   一旦小さなストリップバッファに 16px で描き、2x2 拡大して LCD へ転送する
//!   (`jp2x_*`)。数値 (体温/血圧/結果) は Logisoso42 を直接使う。
//! - ステータスバーは小さいまま (FONT_6X10 / 18px 高)。毎秒更新は
//!   背景色付きテキストスタイルでの上書き描画のみ行い、全面クリアしない
//!   (時計の blink 防止)。
//! - 画面向き (ROTATE) に追従するため、幅・高さは実行時に取得する。

use alc_hub_core::device::DeviceKind;
use alc_hub_core::layout::{fmt_uptime, qr_scale, wrap_chars};
use alc_hub_core::vitals;
use embedded_graphics::{
    mono_font::{ascii::FONT_6X10, MonoTextStyle, MonoTextStyleBuilder},
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{Circle, Line, PrimitiveStyle, Rectangle},
    text::Text,
};
use qrcodegen::{QrCode, QrCodeEcc};
use u8g2_fonts::{
    fonts,
    types::{FontColor, HorizontalAlignment, VerticalPosition},
    FontRenderer,
};

use super::Screen;
use alc_hub_board::display::Cs3Display;
use alc_hub_common::{config, status::HubStatus};

const BAR_H: i32 = 18;

const C_BG: Rgb565 = Rgb565::BLACK;
const C_BAR_BG: Rgb565 = Rgb565::CSS_DARK_SLATE_GRAY;
const C_TEXT: Rgb565 = Rgb565::WHITE;
const C_MUTED: Rgb565 = Rgb565::CSS_GRAY;
const C_ACCENT: Rgb565 = Rgb565::CSS_DEEP_SKY_BLUE;
const C_OK: Rgb565 = Rgb565::CSS_LIME_GREEN;
const C_NG: Rgb565 = Rgb565::CSS_ORANGE_RED;
const C_BTN_TOP: Rgb565 = Rgb565::CSS_MIDNIGHT_BLUE;
const C_BTN_BOTTOM: Rgb565 = Rgb565::CSS_DARK_SLATE_GRAY;

const JP16: FontRenderer = FontRenderer::new::<fonts::u8g2_font_b16_b_t_japanese2>();
const BIG42: FontRenderer = FontRenderer::new::<fonts::u8g2_font_logisoso42_tr>();

/// 2 倍拡大描画の 1 行あたり高さ (16px フォント + 余白 → x2)
const LINE2X_H: i32 = 40;

/// 現在の画面向きでの (幅, 高さ)
fn dims(d: &Cs3Display) -> (i32, i32) {
    let size = d.bounding_box().size;
    (size.width as i32, size.height as i32)
}

// ---------------------------------------------------------------------------
// 2 倍拡大テキスト描画
// ---------------------------------------------------------------------------

const STRIP_W: usize = 160;
const STRIP_H: usize = 24;
/// 描画基準位置の上に確保するヘッドルーム (ソース px)。
/// u8g2 の VerticalPosition::Top はフォントのアセント値基準のため、
/// 濁点・半濁点や一部グリフはその上にはみ出すことがある。ヘッドルーム無しだと
/// ストリップ上端でクリップされ「日本語の上が少し見切れる」症状になる
const STRIP_HEADROOM: usize = 4;

/// JP16 を一旦描くオフスクリーンの小バッファ
struct Strip {
    buf: Vec<Rgb565>,
    bg: Rgb565,
}

impl Strip {
    fn new(bg: Rgb565) -> Self {
        Self {
            buf: vec![bg; STRIP_W * STRIP_H],
            bg,
        }
    }
}

impl OriginDimensions for Strip {
    fn size(&self) -> Size {
        Size::new(STRIP_W as u32, STRIP_H as u32)
    }
}

impl DrawTarget for Strip {
    type Color = Rgb565;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Rgb565>>,
    {
        for Pixel(p, c) in pixels {
            if (0..STRIP_W as i32).contains(&p.x) && (0..STRIP_H as i32).contains(&p.y) {
                self.buf[p.y as usize * STRIP_W + p.x as usize] = c;
            }
        }
        Ok(())
    }
}

/// 中央揃えの 2 倍拡大テキスト 1 行 (実効 32px)
fn jp2x_center(d: &mut Cs3Display, s: &str, y: i32, fg: Rgb565, bg: Rgb565) {
    let (w, _) = dims(d);
    let mut strip = Strip::new(bg);
    let _ = JP16.render_aligned(
        s,
        Point::new((STRIP_W as i32) / 2, STRIP_HEADROOM as i32),
        VerticalPosition::Top,
        HorizontalAlignment::Center,
        FontColor::Transparent(fg),
        &mut strip,
    );

    let dest_w = w.min((STRIP_W * 2) as i32);
    let src_w = (dest_w / 2) as usize;
    let src_x0 = (STRIP_W - src_w) / 2;
    let x0 = (w - dest_w) / 2;
    let mut row = vec![bg; dest_w as usize];
    for sy in 0..STRIP_H {
        let src_row = &strip.buf[sy * STRIP_W + src_x0..sy * STRIP_W + src_x0 + src_w];
        // 文字の無い行はスキップ (背景は全画面描画時に塗り済み)
        if src_row.iter().all(|c| *c == strip.bg) {
            continue;
        }
        // ヘッドルーム行は呼び出し側の y (= 文字の上端想定) より上へ描く
        let dy_base = y + ((sy as i32) - (STRIP_HEADROOM as i32)) * 2;
        if dy_base < 0 {
            continue;
        }
        for (sx, c) in src_row.iter().enumerate() {
            row[sx * 2] = *c;
            row[sx * 2 + 1] = *c;
        }
        for dy in 0..2 {
            let _ = d.fill_contiguous(
                &Rectangle::new(Point::new(x0, dy_base + dy), Size::new(dest_w as u32, 1)),
                row.iter().copied(),
            );
        }
    }
}

/// 左寄せの 2 倍拡大テキスト 1 行 (実効 32px)
fn jp2x_left(d: &mut Cs3Display, s: &str, x: i32, y: i32, fg: Rgb565, bg: Rgb565) {
    let (w, _) = dims(d);
    let mut strip = Strip::new(bg);
    let _ = JP16.render_aligned(
        s,
        Point::new(0, STRIP_HEADROOM as i32),
        VerticalPosition::Top,
        HorizontalAlignment::Left,
        FontColor::Transparent(fg),
        &mut strip,
    );

    let dest_w = (w - x).min((STRIP_W * 2) as i32);
    if dest_w <= 0 {
        return;
    }
    let src_w = (dest_w / 2) as usize;
    let mut row = vec![bg; dest_w as usize];
    for sy in 0..STRIP_H {
        let src_row = &strip.buf[sy * STRIP_W..sy * STRIP_W + src_w];
        if src_row.iter().all(|c| *c == strip.bg) {
            continue;
        }
        let dy_base = y + ((sy as i32) - (STRIP_HEADROOM as i32)) * 2;
        if dy_base < 0 {
            continue;
        }
        for (sx, c) in src_row.iter().enumerate() {
            row[sx * 2] = *c;
            row[sx * 2 + 1] = *c;
        }
        for dy in 0..2 {
            let _ = d.fill_contiguous(
                &Rectangle::new(Point::new(x, dy_base + dy), Size::new(dest_w as u32, 1)),
                row.iter().copied(),
            );
        }
    }
}

/// 2 倍拡大テキストを max_chars で折り返して描画。次の y を返す
fn jp2x_lines(d: &mut Cs3Display, s: &str, y: i32, fg: Rgb565, bg: Rgb565, max_chars: usize) -> i32 {
    let mut y = y;
    for line in wrap_chars(s, max_chars) {
        jp2x_center(d, &line, y, fg, bg);
        y += LINE2X_H;
    }
    y
}

// ---------------------------------------------------------------------------
// 基本ヘルパ
// ---------------------------------------------------------------------------

/// 16px テキスト描画 (エラーは無視 — SPI 書き込み失敗時に UI を止めない)
fn text(
    d: &mut Cs3Display,
    font: &FontRenderer,
    s: &str,
    x: i32,
    y: i32,
    color: Rgb565,
    align: HorizontalAlignment,
) -> Option<Rectangle> {
    font.render_aligned(
        s,
        Point::new(x, y),
        VerticalPosition::Top,
        align,
        FontColor::Transparent(color),
        d,
    )
    .ok()
    .flatten()
}

fn jp_center(d: &mut Cs3Display, s: &str, y: i32, color: Rgb565) {
    let (w, _) = dims(d);
    text(d, &JP16, s, w / 2, y, color, HorizontalAlignment::Center);
}

fn fill(d: &mut Cs3Display, x: i32, y: i32, w: u32, h: u32, color: Rgb565) {
    let _ = d.fill_solid(&Rectangle::new(Point::new(x, y), Size::new(w, h)), color);
}

fn clear(d: &mut Cs3Display) {
    let _ = d.clear(C_BG);
}

// ---------------------------------------------------------------------------
// 全画面描画
// ---------------------------------------------------------------------------

pub fn draw_full(d: &mut Cs3Display, screen: &Screen, st: &HubStatus, now: u64, entered: u64) {
    match screen {
        Screen::Idle => draw_idle(d),
        Screen::Menu => draw_menu(d),
        Screen::Qr {
            payload,
            timeout_ms,
        } => {
            let remain_s = timeout_ms.saturating_sub(now.saturating_sub(entered)) / 1000;
            draw_qr(d, payload, remain_s);
        }
        Screen::Measuring {
            temp, bp, alcohol, ..
        } => draw_tenko(d, *temp, *bp, alcohol),
        Screen::Result { ok, value } => draw_result(d, *ok, value),
        Screen::Error { message } => draw_error(d, message),
        Screen::Temperature { celsius } => draw_temperature(d, *celsius),
        Screen::BloodPressure {
            systolic,
            diastolic,
            pulse,
        } => draw_blood_pressure(d, *systolic, *diastolic, *pulse),
        Screen::Log => draw_log(d, st, now),
    }
    draw_status_bar(d, st, now);
}

pub fn draw_boot(d: &mut Cs3Display) {
    clear(d);
    jp2x_center(d, "alc-hub CoreS3", 70, C_TEXT, C_BG);
    jp_center(
        d,
        &format!("起動中... v{}", config::FIRMWARE_VERSION),
        130,
        C_MUTED,
    );
}

/// 待機画面: NFC カード待ち
fn draw_idle(d: &mut Cs3Display) {
    let (_, h) = dims(d);
    clear(d);
    jp_center(d, "NFC 待機中", BAR_H + 6, C_MUTED);
    jp2x_lines(d, "カードをかざしてください", 66, C_TEXT, C_BG, 8);
    jp_center(d, "タップでメニュー", h - 24, C_MUTED);
}

/// メニュー: 上半分 = 点呼 / 下半分 = ログ確認
fn draw_menu(d: &mut Cs3Display) {
    let (w, h) = dims(d);
    clear(d);
    let zone_h = (h - BAR_H) / 2;
    fill(d, 0, BAR_H, w as u32, zone_h as u32, C_BTN_TOP);
    fill(
        d,
        0,
        BAR_H + zone_h,
        w as u32,
        (h - BAR_H - zone_h) as u32,
        C_BTN_BOTTOM,
    );
    fill(d, 0, BAR_H + zone_h - 1, w as u32, 2, C_BG); // 境界線
    jp2x_center(d, "点呼", BAR_H + zone_h / 2 - 18, C_TEXT, C_BTN_TOP);
    jp2x_center(
        d,
        "ログ確認",
        BAR_H + zone_h + zone_h / 2 - 18,
        C_TEXT,
        C_BTN_BOTTOM,
    );
}

fn draw_qr(d: &mut Cs3Display, payload: &str, remain_s: u64) {
    let (w, h) = dims(d);
    clear(d);
    // QR は白背景必須 (クワイエットゾーン確保)
    fill(d, 0, BAR_H, w as u32, (h - BAR_H) as u32, Rgb565::WHITE);

    match QrCode::encode_text(payload, QrCodeEcc::Medium) {
        Ok(qr) => {
            let size = qr.size(); // モジュール数 (正方形)
            let avail = (h - BAR_H - 40).min(w - 16); // 下部の案内文スペースを除く
            let scale = qr_scale(avail, size);
            let px = size * scale;
            let x0 = (w - px) / 2;
            let y0 = BAR_H + 6;
            for y in 0..size {
                for x in 0..size {
                    if qr.get_module(x, y) {
                        fill(
                            d,
                            x0 + x * scale,
                            y0 + y * scale,
                            scale as u32,
                            scale as u32,
                            Rgb565::BLACK,
                        );
                    }
                }
            }
        }
        Err(e) => {
            log::error!("QR 生成失敗: {e:?}");
            text(
                d,
                &JP16,
                "QRコードを生成できませんでした",
                w / 2,
                110,
                Rgb565::BLACK,
                HorizontalAlignment::Center,
            );
        }
    }

    text(
        d,
        &JP16,
        "読み取り機にかざしてください",
        w / 2,
        h - 26,
        Rgb565::BLACK,
        HorizontalAlignment::Center,
    );
    draw_qr_countdown(d, remain_s);
}

/// QR 画面右上の残り秒数 (毎秒の部分更新)
pub fn draw_qr_countdown(d: &mut Cs3Display, remain_s: u64) {
    let (w, _) = dims(d);
    fill(d, w - 100, BAR_H + 2, 98, 20, Rgb565::WHITE);
    text(
        d,
        &JP16,
        &format!("残り {remain_s}秒"),
        w - 4,
        BAR_H + 4,
        Rgb565::BLACK,
        HorizontalAlignment::Right,
    );
}

// --- 点呼画面レイアウト (基準 320x240) ---
// 3 段構成 (上から 体温 / 血圧 / アルコール)。ラベルは左寄せ、値は右寄せ。
// 各段 74px (= (240 - ステータスバー 18) / 3)
const TENKO_ROW_H: i32 = 74;
const TENKO_TEMP_Y: i32 = BAR_H;
const TENKO_BP_Y: i32 = BAR_H + TENKO_ROW_H;
const TENKO_ALC_Y: i32 = BAR_H + TENKO_ROW_H * 2;
const TENKO_LABEL_X: i32 = 4;
/// スピナー中心 X (ラベル 2 文字 = 64px の右外側)
const TENKO_SPIN_X: i32 = 92;

/// 未計測欄の「計測待ち」(4 文字 = 128px) を右寄せで描く
fn tenko_waiting(d: &mut Cs3Display, row_y: i32) {
    let (w, _) = dims(d);
    jp2x_left(d, "計測待ち", w - 136, row_y + 20, C_MUTED, C_BG);
}

/// 点呼画面: 体温 / 血圧 / アルコールを同一画面で計測・確認する (3 段)。
/// 未計測の欄は「計測待ち」。取得中スピナーは draw_tenko_spinner (部分更新)
fn draw_tenko(
    d: &mut Cs3Display,
    temp: Option<f32>,
    bp: Option<(f32, f32, Option<f32>)>,
    alcohol: &Option<(bool, String)>,
) {
    let (w, _) = dims(d);
    clear(d);

    // 段の区切り線
    fill(d, 8, TENKO_BP_Y - 1, (w - 16) as u32, 2, C_BAR_BG);
    fill(d, 8, TENKO_ALC_Y - 1, (w - 16) as u32, 2, C_BAR_BG);

    // --- 上段: 体温 ---
    jp2x_left(d, "体温", TENKO_LABEL_X, TENKO_TEMP_Y + 20, C_ACCENT, C_BG);
    match temp {
        Some(celsius) => {
            // ℃ の幅の分だけ左に寄せて右端を揃える
            let rect = text(
                d,
                &BIG42,
                &vitals::temp_value(celsius),
                w - 40,
                TENKO_TEMP_Y + 14,
                C_TEXT,
                HorizontalAlignment::Right,
            );
            if let Some(r) = rect {
                let ux = r.top_left.x + r.size.width as i32 + 8;
                text(d, &JP16, "℃", ux, TENKO_TEMP_Y + 36, C_TEXT, HorizontalAlignment::Left);
            }
        }
        None => tenko_waiting(d, TENKO_TEMP_Y),
    }

    // --- 中段: 血圧 ---
    jp2x_left(d, "血圧", TENKO_LABEL_X, TENKO_BP_Y + 8, C_ACCENT, C_BG);
    match bp {
        Some((systolic, diastolic, pulse)) => {
            // 収縮期 / 拡張期 は別々に描き '/' は線で手描き (draw_blood_pressure
            // と同じ理由: BIG42 に無いグリフ混在で全体が消えるのを回避)
            let y = TENKO_BP_Y + 14;
            let pivot = w - 96;
            text(
                d,
                &BIG42,
                &format!("{systolic:.0}"),
                pivot - 12,
                y,
                C_TEXT,
                HorizontalAlignment::Right,
            );
            text(
                d,
                &BIG42,
                &format!("{diastolic:.0}"),
                pivot + 12,
                y,
                C_TEXT,
                HorizontalAlignment::Left,
            );
            let _ = Line::new(Point::new(pivot - 7, y + 44), Point::new(pivot + 7, y))
                .into_styled(PrimitiveStyle::with_stroke(C_TEXT, 4))
                .draw(d);
            // 脈拍はラベルの下 (左列) に小さく
            if let Some(p) = pulse {
                if p > 0.0 {
                    text(
                        d,
                        &JP16,
                        &vitals::pulse_value(p),
                        TENKO_LABEL_X + 4,
                        TENKO_BP_Y + 50,
                        C_MUTED,
                        HorizontalAlignment::Left,
                    );
                }
            }
        }
        None => tenko_waiting(d, TENKO_BP_Y),
    }

    // --- 最下段: アルコール (ホストの RESULT で更新。表示のみ) ---
    jp2x_left(d, "アルコール", TENKO_LABEL_X, TENKO_ALC_Y + 20, C_ACCENT, C_BG);
    match alcohol {
        Some((ok, value)) => {
            let color = if *ok { C_OK } else { C_NG };
            if value.is_empty() {
                // 値なし (RESULT OK/NG のみ) → 判定だけ表示
                jp2x_left(
                    d,
                    if *ok { "OK" } else { "NG" },
                    w - 72,
                    TENKO_ALC_Y + 20,
                    color,
                    C_BG,
                );
            } else {
                text(
                    d,
                    &BIG42,
                    value,
                    w - 8,
                    TENKO_ALC_Y + 8,
                    color,
                    HorizontalAlignment::Right,
                );
                text(
                    d,
                    &JP16,
                    "mg/L",
                    w - 8,
                    TENKO_ALC_Y + 54,
                    C_MUTED,
                    HorizontalAlignment::Right,
                );
            }
        }
        None => tenko_waiting(d, TENKO_ALC_Y),
    }
}

/// 点呼画面: 取得中機器のラベル横ミニスピナー (部分更新)。8 ドット。
/// BLE 接続開始 (BleAcquiring) 中のみ UI ループが 150ms ごとに呼ぶ
pub fn draw_tenko_spinner(d: &mut Cs3Display, kind: DeviceKind, phase: u8) {
    let cx = TENKO_SPIN_X as f32;
    // ラベル (32px 高) の縦中央に合わせる
    let cy = match kind {
        DeviceKind::Thermometer => TENKO_TEMP_Y + 36,
        DeviceKind::BloodPressure => TENKO_BP_Y + 24,
    } as f32;
    const R: f32 = 11.0;
    for i in 0..8u8 {
        let ang = core::f32::consts::TAU * f32::from(i) / 8.0;
        let x = cx + R * ang.cos();
        let y = cy + R * ang.sin();
        let color = if i == phase { C_ACCENT } else { C_BAR_BG };
        let _ = Circle::with_center(Point::new(x as i32, y as i32), 5)
            .into_styled(PrimitiveStyle::with_fill(color))
            .draw(d);
    }
}

fn draw_result(d: &mut Cs3Display, ok: bool, value: &str) {
    let (w, h) = dims(d);
    clear(d);
    let (label, color, note) = if ok {
        ("OK", C_OK, "おつかれさまでした")
    } else {
        ("NG", C_NG, "再測定してください")
    };
    text(d, &BIG42, label, w / 2, BAR_H + 6, color, HorizontalAlignment::Center);
    if !value.is_empty() {
        let rect = text(
            d,
            &BIG42,
            value,
            w / 2,
            76,
            C_TEXT,
            HorizontalAlignment::Center,
        );
        if let Some(r) = rect {
            let ux = r.top_left.x + r.size.width as i32 + 6;
            text(d, &JP16, "mg/L", ux, 96, C_MUTED, HorizontalAlignment::Left);
        }
    }
    jp2x_lines(d, note, 134, C_TEXT, C_BG, 9);
    jp_center(d, "タップで待機画面へ", h - 24, C_MUTED);
}

fn draw_error(d: &mut Cs3Display, message: &str) {
    let (w, h) = dims(d);
    clear(d);
    fill(d, 0, BAR_H, w as u32, 42, C_NG);
    jp2x_center(d, "エラー", BAR_H + 4, C_TEXT, C_NG);
    let msg = if message.is_empty() {
        "不明なエラー"
    } else {
        message
    };
    jp_center(d, msg, 110, C_TEXT);
    jp_center(d, "タップで戻る", h - 24, C_MUTED);
}

/// 体温表示 (BLE 体温計)
fn draw_temperature(d: &mut Cs3Display, celsius: f32) {
    let (w, h) = dims(d);
    clear(d);
    jp2x_center(d, "体温", BAR_H + 6, C_ACCENT, C_BG);
    let rect = text(
        d,
        &BIG42,
        &vitals::temp_value(celsius),
        w / 2,
        94,
        C_TEXT,
        HorizontalAlignment::Center,
    );
    if let Some(r) = rect {
        let ux = r.top_left.x + r.size.width as i32 + 8;
        text(d, &JP16, "℃", ux, 116, C_TEXT, HorizontalAlignment::Left);
    }
    jp_center(d, "タップで戻る", h - 24, C_MUTED);
}

/// 血圧表示 (BLE 血圧計)
fn draw_blood_pressure(d: &mut Cs3Display, systolic: f32, diastolic: f32, pulse: Option<f32>) {
    let (w, h) = dims(d);
    clear(d);
    jp2x_center(d, "血圧", BAR_H + 6, C_ACCENT, C_BG);

    // 収縮期 / 拡張期 を別々の BIG42 描画にし、区切りは線で手描きする。
    // u8g2 の render は文字列中に 1 文字でも欠けたグリフがあると全体が
    // 描画されないため、'/' がフォントに無い場合に数字ごと消える問題を回避
    // (体温は数字+'.' のみで表示できていた)。
    let y = 78;
    let cx = w / 2;
    // 収縮期: 中央やや左に右揃え / 拡張期: 中央やや右に左揃え
    text(
        d,
        &BIG42,
        &format!("{systolic:.0}"),
        cx - 16,
        y,
        C_TEXT,
        HorizontalAlignment::Right,
    );
    text(
        d,
        &BIG42,
        &format!("{diastolic:.0}"),
        cx + 16,
        y,
        C_TEXT,
        HorizontalAlignment::Left,
    );
    // 区切りスラッシュ (線)
    let _ = Line::new(Point::new(cx - 7, y + 44), Point::new(cx + 7, y))
        .into_styled(PrimitiveStyle::with_stroke(C_TEXT, 4))
        .draw(d);
    jp_center(d, "mmHg", y + 52, C_MUTED);

    match pulse {
        Some(p) if p > 0.0 => jp2x_center(d, &vitals::pulse_value(p), 150, C_TEXT, C_BG),
        _ => {}
    }
    jp_center(d, "タップで戻る", h - 24, C_MUTED);
}

/// イベントログ + 機器ステータス (文字サイズは小さいまま・余白圧縮)
fn draw_log(d: &mut Cs3Display, st: &HubStatus, now: u64) {
    let (_, h) = dims(d);
    clear(d);

    let flag = |b: bool| if b { "○" } else { "×" };
    let summary = format!(
        "LAN{} 232{} BLE{} WiFi{}  v{}",
        flag(st.lan_link),
        flag(st.rs232_active(now, config::RS232_ACTIVE_WINDOW_MS)),
        flag(st.ble_connected),
        flag(st.wifi_connected),
        config::FIRMWARE_VERSION,
    );
    text(d, &JP16, &summary, 6, BAR_H + 2, C_ACCENT, HorizontalAlignment::Left);
    if st.wifi_connected {
        text(
            d,
            &JP16,
            &format!("IP {}", st.wifi_ip),
            6,
            BAR_H + 22,
            C_MUTED,
            HorizontalAlignment::Left,
        );
    }

    let mut y = BAR_H + 44;
    if st.events.is_empty() {
        text(d, &JP16, "イベントなし", 6, y, C_MUTED, HorizontalAlignment::Left);
    } else {
        // 新しいものを上に
        for line in st.events.iter().rev() {
            if y > h - 40 {
                break;
            }
            text(d, &JP16, line, 6, y, C_TEXT, HorizontalAlignment::Left);
            y += 20;
        }
    }
    jp_center(d, "タップで戻る", h - 20, C_MUTED);
}

// ---------------------------------------------------------------------------
// ステータスバー (全画面共通)
// ---------------------------------------------------------------------------

const BAR_ITEMS_X: [i32; 4] = [6, 50, 94, 138];

fn bar_items(st: &HubStatus, now: u64) -> [(&'static str, bool); 4] {
    [
        ("LAN", st.lan_link),
        ("232", st.rs232_active(now, config::RS232_ACTIVE_WINDOW_MS)),
        ("BLE", st.ble_connected),
        ("WiFi", st.wifi_connected),
    ]
}

fn draw_bar_dots(d: &mut Cs3Display, st: &HubStatus, now: u64) {
    for (x, (_, on)) in BAR_ITEMS_X.iter().zip(bar_items(st, now)) {
        let color = if on { C_OK } else { Rgb565::CSS_DARK_RED };
        let _ = Circle::with_center(Point::new(x + 3, BAR_H / 2), 6)
            .into_styled(PrimitiveStyle::with_fill(color))
            .draw(d);
    }
}

fn draw_bar_clock(d: &mut Cs3Display, now: u64) {
    let (w, _) = dims(d);
    // 背景色付きスタイル: グリフごとに背景+文字を同時描画するため、
    // 事前クリア不要で上書きでき、毎秒更新しても blink しない
    let style = MonoTextStyleBuilder::new()
        .font(&FONT_6X10)
        .text_color(C_TEXT)
        .background_color(C_BAR_BG)
        .build();
    let up = fmt_uptime(now);
    let _ = Text::new(&up, Point::new(w - 6 - up.len() as i32 * 6, 13), style).draw(d);
}

/// 全面描画 (画面遷移時のみ)
pub fn draw_status_bar(d: &mut Cs3Display, st: &HubStatus, now: u64) {
    let (w, _) = dims(d);
    fill(d, 0, 0, w as u32, BAR_H as u32, C_BAR_BG);
    let style = MonoTextStyle::new(&FONT_6X10, C_TEXT);
    for (x, (label, _)) in BAR_ITEMS_X.iter().zip(bar_items(st, now)) {
        let _ = Text::new(label, Point::new(x + 9, 13), style).draw(d);
    }
    draw_bar_dots(d, st, now);
    draw_bar_clock(d, now);
}

/// 毎秒の部分更新 (バー全面は塗らない — blink 防止)
pub fn update_status_bar(d: &mut Cs3Display, st: &HubStatus, now: u64) {
    draw_bar_dots(d, st, now);
    draw_bar_clock(d, now);
}
