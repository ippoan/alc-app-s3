//! ES8311 codec + NS4150B アンプ (Atom VoiceS3R の内蔵スピーカー、issue #154)。
//!
//! CoreS3 の AW88298 ([`crate::speaker::init_amp`]) に対応する**ボード依存部分だけ**の
//! モジュール。再生ロジック (`Sound` / `start_player` / `beep` / `feed_silence`) は
//! [`crate::speaker`] と共有する — 差はコーデックの初期化とアンプ有効化の 2 点だけ
//! (plan/standing-devices.md §2.3)。
//!
//! # 配線 (すべて内部ピン。plan §3.1)
//!
//! - **I2C: SDA=G45 / SCL=G0** (VoiceS3R の内蔵バス)。ES8311 は **0x18**
//! - **I2S: BCLK=G17 / WS=G3 / DOUT=G48**
//! - **NS4150B の有効化: G18** (`NS4150_CTR`) を output HIGH
//!
//! # ★ MCLK (G11) は要らない
//!
//! レジスタ 0x01 = 0xB5 が **「MCLK は BCLK から取る」**指定なので、**MCLK ピンを
//! 配線しなくても鳴る**。M5Unified の VoiceS3R 定義 (`M5Unified.cpp` の
//! `board_M5AtomVoiceS3R` 分岐) でも **スピーカー側は `spk_cfg.pin_mck` が
//! コメントアウトされている** (`pin_mck = GPIO_NUM_11` を設定しているのは
//! マイク側だけ)。おかげで [`crate::speaker::Speaker::new`] を CoreS3 と同じ
//! 引数のまま使える。
//!
//! # レジスタ列の出どころ
//!
//! M5Unified (MIT) の `_speaker_enabled_cb_atom_echos3r` の移植。
//! **AW88298 と違い ES8311 のレジスタは 8bit** (AW88298 は 16bit ビッグエンディアン)。
//!
//! # CoreS3 で踏んだ罠はここでも効く (Refs #102)
//!
//! - サンプルレートは **48kHz 固定** ([`crate::speaker`] の `SAMPLE_RATE_HZ`)
//! - **クロックを流してから初期化する** — `Speaker::new` → `feed_silence` →
//!   [`init_amp`] の順。新 I2S ドライバは FIFO 空で BCK を止めるので、
//!   `tx_enable()` だけではクロックが出ず PLL がロックしない

use anyhow::Result;
use esp_idf_svc::hal::delay::{FreeRtos, BLOCK};
use esp_idf_svc::hal::gpio::{AnyOutputPin, Output, PinDriver};
use esp_idf_svc::hal::i2c::I2cDriver;

/// ES8311 の I2C アドレス (CE ピン Low 側)。VoiceS3R は内蔵バスのこちら。
/// M5Unified が VoiceS3R の判別にこのアドレスの probe を使っている
pub const ES8311_ADDR: u8 = 0x18;

/// LP5562 (RGB LED ドライバ) の I2C アドレス。**VoiceS3R に実装されているかは未確定**で、
/// AtomS3R では実装されている (issue #151)。同じ内蔵バスなので [`probe_bus`] のついでに見る
pub const LP5562_ADDR: u8 = 0x30;

/// M5Unified `_speaker_enabled_cb_atom_echos3r` の `enabled_bulk_data` (reg, value)。
/// **順番に意味がある** — 0x00 のリセット/CSM 起動が先頭でないと以降が効かない
const ENABLE_SEQ: [(u8, u8); 8] = [
    (0x00, 0x80), // RESET / CSM POWER ON
    (0x01, 0xB5), // CLOCK_MANAGER: MCLK=BCLK (★ MCLK ピンを使わない指定)
    (0x02, 0x18), // CLOCK_MANAGER: MULT_PRE=3
    (0x0D, 0x01), // SYSTEM: アナログ回路を起こす
    (0x12, 0x00), // SYSTEM: DAC を起こす (既定値ではない)
    (0x13, 0x10), // SYSTEM: HP ドライブへの出力を有効化 (既定値ではない)
    // DAC 音量。ES8311 は 0xBF = 0dB、1 step 0.5dB (0xFF = +32dB、0x00 = ミュート)。
    // M5Unified の移植では 0xFF (フル) だったが、実機では大きすぎて
    // 「音量を下げないとタップできない」(#155、2026-09-06) → 0xB0 (= −7.5dB、
    // フルから −39.5dB)。実機で聞いて調整する
    (0x32, 0xB0),
    (0x37, 0x08), // DAC: イコライザをバイパス (既定値ではない)
];

fn write_reg(i2c: &mut I2cDriver, reg: u8, value: u8) -> Result<()> {
    i2c.write(ES8311_ADDR, &[reg, value], BLOCK)?;
    Ok(())
}

fn read_reg(i2c: &mut I2cDriver, reg: u8) -> Result<u8> {
    let mut buf = [0u8; 1];
    i2c.write_read(ES8311_ADDR, &[reg], &mut buf, BLOCK)?;
    Ok(buf[0])
}

/// 内蔵 I2C バスに居るデバイスを列挙してログへ出す (診断)。
///
/// 目的は 2 つ。**① ES8311 (0x18) が見えるか** — 見えないなら配線かバス番号が違う。
/// **② LP5562 (0x30) が居るか** — 居なければ「VoiceS3R に RGB LED は無い」が確定する
/// (issue #151 で未確定のまま残した点。居ても LED を点ける実装は別の話)
pub fn probe_bus(i2c: &mut I2cDriver) {
    let mut found = Vec::new();
    for addr in 0x08u8..0x78 {
        // 1 バイト読めれば ACK した = 居る。読めない (NACK) なら居ない
        let mut buf = [0u8; 1];
        if i2c.read(addr, &mut buf, 20).is_ok() {
            found.push(addr);
        }
    }
    let list = found
        .iter()
        .map(|a| format!("0x{a:02X}"))
        .collect::<Vec<_>>()
        .join(",");
    log::info!("es8311: I2C scan = [{list}]");
    println!(
        "EVT I2C_SCAN devices={} es8311={} lp5562={}",
        found.len(),
        u8::from(found.contains(&ES8311_ADDR)),
        u8::from(found.contains(&LP5562_ADDR)),
    );
}

/// ES8311 を I2S 入力・フル音量で有効化し、NS4150B アンプを有効にする。
///
/// **[`crate::speaker::Speaker::new`] + `feed_silence` で BCK/WS を実際に流した後に
/// 呼ぶこと** (Refs #102 — クロック無しで初期化すると PLL がロックしない)。
///
/// `amp_en` は NS4150B の有効化ピン (VoiceS3R は **G18**)。`PinDriver` を返して
/// 呼び出し元に持たせる — **drop すると GPIO が解放されて出力が落ちる**ため、
/// 鳴らしているあいだ生かしておく必要がある
pub fn init_amp(
    i2c: &mut I2cDriver,
    amp_en: AnyOutputPin<'static>,
) -> Result<PinDriver<'static, Output>> {
    for (reg, value) in ENABLE_SEQ {
        write_reg(i2c, reg, value)?;
    }
    // CSM (チャージポンプ / 電源シーケンサ) の立ち上がり待ち。AW88298 のリセット後と
    // 同じく数 ms で足りるが、鳴らないときの切り分けを減らすため余裕を取る
    FreeRtos::delay_ms(10);
    // 診断: 0x00 (RESET/CSM) と 0x32 (DAC 音量) を読み戻し、書けているか確認する。
    // **読めない = I2C は通っているが別のデバイス**という切り分けができる
    match (read_reg(i2c, 0x00), read_reg(i2c, 0x32)) {
        (Ok(r00), Ok(r32)) => log::info!("es8311: 初期化後 [0x00]=0x{r00:02X} [0x32]=0x{r32:02X}"),
        _ => log::warn!("es8311: 初期化後のレジスタ読み戻しに失敗"),
    }

    // NS4150B の有効化 (G18 を HIGH)。**codec を起こしてから最後に上げる** —
    // 先に上げるとコーデック初期化中のノイズがそのままスピーカーに出る
    let mut en = PinDriver::output(amp_en)?;
    en.set_high()?;
    log::info!("es8311: NS4150B アンプ有効化");
    Ok(en)
}

/// ES8311 の主要レジスタをログへダンプする (無音時の切り分け用)。
/// AW88298 の [`crate::speaker::dump_regs`] と同じ役割 — 鳴っている状態と
/// 鳴らない状態の差分レジスタを突き合わせるためのもの (Refs #102)
pub fn dump_regs(i2c: &mut I2cDriver) {
    for reg in (0x00u8..=0x0Fu8).chain(0x12..=0x14).chain([0x32, 0x37]) {
        match read_reg(i2c, reg) {
            Ok(v) => log::info!("es8311: [0x{reg:02X}]=0x{v:02X}"),
            Err(e) => log::warn!("es8311: [0x{reg:02X}] 読み出し失敗: {e:#}"),
        }
    }
}
