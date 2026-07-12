//! 各画面の描画。320x240 (上部 24px はステータスバー)。
//!
//! 日本語は u8g2 の JIS 収録フォント (b16, 16px)、大きな英数字は
//! Logisoso32 で描画する。フレームバッファを持たず LCD へ直接描画するため、
//! 全画面再描画は画面遷移時のみ・以降は部分更新とする。

use embedded_graphics::{
    mono_font::{ascii::FONT_6X10, MonoTextStyle},
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{Circle, PrimitiveStyle, Rectangle},
    text::Text,
};
use qrcodegen::{QrCode, QrCodeEcc};
use u8g2_fonts::{
    fonts,
    types::{FontColor, HorizontalAlignment, VerticalPosition},
    FontRenderer,
};

use super::Screen;
use crate::{
    board::display::{Cs3Display, LCD_H as H, LCD_W as W},
    config,
    status::HubStatus,
};

const BAR_H: i32 = 24;

const C_BG: Rgb565 = Rgb565::BLACK;
const C_BAR_BG: Rgb565 = Rgb565::CSS_DARK_SLATE_GRAY;
const C_TEXT: Rgb565 = Rgb565::WHITE;
const C_MUTED: Rgb565 = Rgb565::CSS_GRAY;
const C_ACCENT: Rgb565 = Rgb565::CSS_DEEP_SKY_BLUE;
const C_OK: Rgb565 = Rgb565::CSS_LIME_GREEN;
const C_NG: Rgb565 = Rgb565::CSS_ORANGE_RED;

const JP16: FontRenderer = FontRenderer::new::<fonts::u8g2_font_b16_b_t_japanese2>();
const BIG32: FontRenderer = FontRenderer::new::<fonts::u8g2_font_logisoso32_tr>();

/// テキスト描画 (エラーは無視 — SPI 書き込み失敗時に UI を止めない)
fn text(
    d: &mut Cs3Display,
    font: &FontRenderer,
    s: &str,
    x: i32,
    y: i32,
    color: Rgb565,
    align: HorizontalAlignment,
) {
    let _ = font.render_aligned(
        s,
        Point::new(x, y),
        VerticalPosition::Top,
        align,
        FontColor::Transparent(color),
        d,
    );
}

fn jp_center(d: &mut Cs3Display, s: &str, y: i32, color: Rgb565) {
    text(d, &JP16, s, W / 2, y, color, HorizontalAlignment::Center);
}

fn fill(d: &mut Cs3Display, x: i32, y: i32, w: u32, h: u32, color: Rgb565) {
    let _ = d.fill_solid(&Rectangle::new(Point::new(x, y), Size::new(w, h)), color);
}

fn clear(d: &mut Cs3Display) {
    let _ = d.clear(C_BG);
}

fn fmt_uptime(now_ms: u64) -> String {
    let s = now_ms / 1000;
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

// ---------------------------------------------------------------------------
// 全画面描画
// ---------------------------------------------------------------------------

pub fn draw_full(d: &mut Cs3Display, screen: &Screen, st: &HubStatus, now: u64, entered: u64) {
    match screen {
        Screen::Idle => draw_idle(d),
        Screen::Qr {
            payload,
            timeout_ms,
        } => {
            let remain_s = timeout_ms.saturating_sub(now.saturating_sub(entered)) / 1000;
            draw_qr(d, payload, remain_s);
        }
        Screen::Measuring => draw_measuring(d),
        Screen::Result { ok, value } => draw_result(d, *ok, value),
        Screen::Error { message } => draw_error(d, message),
        Screen::StatusDetail => draw_status_detail(d, st, now),
    }
    draw_status_bar(d, st, now);
}

pub fn draw_boot(d: &mut Cs3Display) {
    clear(d);
    jp_center(d, "alc-hub CoreS3", 90, C_TEXT);
    jp_center(
        d,
        &format!("起動中... v{}", config::FIRMWARE_VERSION),
        130,
        C_MUTED,
    );
}

fn draw_idle(d: &mut Cs3Display) {
    clear(d);
    jp_center(d, "アルコールチェック", 76, C_TEXT);
    jp_center(d, "タブレットで顔認証をしてください", 122, C_ACCENT);
    jp_center(d, "画面タップ: 機器ステータス", 204, C_MUTED);
}

fn draw_qr(d: &mut Cs3Display, payload: &str, remain_s: u64) {
    clear(d);
    // QR は白背景必須 (クワイエットゾーン確保)
    fill(d, 0, BAR_H, W as u32, (H - BAR_H) as u32, Rgb565::WHITE);

    match QrCode::encode_text(payload, QrCodeEcc::Medium) {
        Ok(qr) => {
            let size = qr.size(); // モジュール数 (正方形)
            let avail = H - BAR_H - 44; // 下部の案内文スペースを除く
            let scale = (avail / (size + 2)).clamp(2, 8);
            let px = size * scale;
            let x0 = (W - px) / 2;
            let y0 = BAR_H + 8;
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
                W / 2,
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
        W / 2,
        H - 30,
        Rgb565::BLACK,
        HorizontalAlignment::Center,
    );
    draw_qr_countdown(d, remain_s);
}

/// QR 画面右上の残り秒数 (毎秒の部分更新)
pub fn draw_qr_countdown(d: &mut Cs3Display, remain_s: u64) {
    fill(d, W - 100, BAR_H + 2, 98, 20, Rgb565::WHITE);
    text(
        d,
        &JP16,
        &format!("残り {remain_s}秒"),
        W - 4,
        BAR_H + 4,
        Rgb565::BLACK,
        HorizontalAlignment::Right,
    );
}

fn draw_measuring(d: &mut Cs3Display) {
    clear(d);
    jp_center(d, "測定中...", 56, C_TEXT);
    draw_spinner(d, 0);
    jp_center(d, "FC-1200 に息を吹き込んでください", 196, C_MUTED);
}

/// 測定中スピナー (部分更新)。中心 (W/2, 130)、8 ドット。
pub fn draw_spinner(d: &mut Cs3Display, phase: u8) {
    const CX: f32 = 160.0;
    const CY: f32 = 132.0;
    const R: f32 = 34.0;
    for i in 0..8u8 {
        let ang = core::f32::consts::TAU * f32::from(i) / 8.0;
        let x = CX + R * ang.cos();
        let y = CY + R * ang.sin();
        let color = if i == phase { C_ACCENT } else { C_BAR_BG };
        let _ = Circle::with_center(Point::new(x as i32, y as i32), 12)
            .into_styled(PrimitiveStyle::with_fill(color))
            .draw(d);
    }
}

fn draw_result(d: &mut Cs3Display, ok: bool, value: &str) {
    clear(d);
    let (label, color, note) = if ok {
        ("OK", C_OK, "測定完了 おつかれさまでした")
    } else {
        ("NG", C_NG, "検知しました 再測定してください")
    };
    text(d, &BIG32, label, W / 2, 44, color, HorizontalAlignment::Center);
    if !value.is_empty() {
        text(
            d,
            &BIG32,
            &format!("{value} mg/L"),
            W / 2,
            100,
            C_TEXT,
            HorizontalAlignment::Center,
        );
    }
    jp_center(d, note, 160, C_TEXT);
    jp_center(d, "タップで待機画面へ", 204, C_MUTED);
}

fn draw_error(d: &mut Cs3Display, message: &str) {
    clear(d);
    fill(d, 0, BAR_H, W as u32, 34, C_NG);
    jp_center(d, "エラー", BAR_H + 8, C_TEXT);
    let msg = if message.is_empty() {
        "不明なエラー"
    } else {
        message
    };
    jp_center(d, msg, 116, C_TEXT);
    jp_center(d, "タップで戻る", 204, C_MUTED);
}

fn draw_status_detail(d: &mut Cs3Display, st: &HubStatus, now: u64) {
    clear(d);
    jp_center(d, "機器ステータス", BAR_H + 8, C_ACCENT);

    let rs232 = match st.rs232_last_rx_ms {
        Some(t) => format!("RS232 (FC-1200): 受信 {}秒前", now.saturating_sub(t) / 1000),
        None => "RS232 (FC-1200): 受信なし".to_string(),
    };
    let ble = if st.ble_connected {
        format!("BLE: 接続 ({})", st.ble_device)
    } else {
        "BLE: 未接続 (未実装)".to_string()
    };
    let lan = format!(
        "LAN リンク: {}",
        if st.lan_link { "あり" } else { "なし (未実装)" }
    );

    let rows = [
        lan.as_str(),
        rs232.as_str(),
        ble.as_str(),
    ];
    let mut y = 78;
    for row in rows {
        text(d, &JP16, row, 16, y, C_TEXT, HorizontalAlignment::Left);
        y += 28;
    }
    text(
        d,
        &JP16,
        &format!("FW v{}  稼働 {}", config::FIRMWARE_VERSION, fmt_uptime(now)),
        16,
        y,
        C_MUTED,
        HorizontalAlignment::Left,
    );
    jp_center(d, "タップで戻る", 204, C_MUTED);
}

// ---------------------------------------------------------------------------
// ステータスバー (全画面共通, 毎秒の部分更新)
// ---------------------------------------------------------------------------

pub fn draw_status_bar(d: &mut Cs3Display, st: &HubStatus, now: u64) {
    fill(d, 0, 0, W as u32, BAR_H as u32, C_BAR_BG);
    let style = MonoTextStyle::new(&FONT_6X10, C_TEXT);

    let items = [
        ("LAN", st.lan_link),
        ("232", st.rs232_active(now, config::RS232_ACTIVE_WINDOW_MS)),
        ("BLE", st.ble_connected),
    ];
    let mut x = 8;
    for (label, on) in items {
        let color = if on { C_OK } else { Rgb565::CSS_DARK_RED };
        let _ = Circle::with_center(Point::new(x + 4, BAR_H / 2), 8)
            .into_styled(PrimitiveStyle::with_fill(color))
            .draw(d);
        let _ = Text::new(label, Point::new(x + 12, 16), style).draw(d);
        x += 56;
    }

    let up = fmt_uptime(now);
    let _ = Text::new(
        &up,
        Point::new(W - 6 - up.len() as i32 * 6, 16),
        style,
    )
    .draw(d);
}
