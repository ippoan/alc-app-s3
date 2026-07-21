//! 内蔵スピーカー (AW88298 I2S アンプ) 読み取りビープ (issue #101 PR2)。
//!
//! CoreS3 の内蔵スピーカーは AW88298 (I2C0, addr 0x36) が I2S 信号 (I2S_NUM_1,
//! BCK=G34 / WS=G33 / DOUT=G13) を増幅する構成。アンプの電源 (AXP2101 ALDO1)
//! は `board::power::init` が LCD/タッチ等と一緒に起動時に有効化しているが、
//! AW9523 P0 bit2 (アンプ有効化) は別物 — M5Unified の Speaker.begin() 相当の
//! 明示的な read-modify-write が必要 (2026-07-21 実機で無音を確認し追加)。
//!
//! レジスタ値は M5Unified (`M5Unified.cpp` の `_speaker_enabled_cb_cores3`、
//! MIT license) の CoreS3 初期化シーケンスを移植したもの。
//!
//! I2S DOUT=G13 は LAN Module 13.2 の CS (`lan.rs`) と同一ピンのため、内蔵
//! スピーカーを使う構成では LAN Module を取り外す (issue #101 の前提、
//! `main.rs` の `lan` feature を無効化)。

use anyhow::Result;
use esp_idf_svc::hal::delay::{FreeRtos, BLOCK};
use esp_idf_svc::hal::gpio::AnyIOPin;
use esp_idf_svc::hal::i2c::I2cDriver;
use esp_idf_svc::hal::i2s::{
    config::{DataBitWidth, StdConfig},
    I2sDriver, I2sTx, I2S1,
};

const AW88298_ADDR: u8 = 0x36;
const AW9523_ADDR: u8 = 0x58;
/// AW9523 P0 出力レジスタ。bit2 = アンプ有効化 (M5Unified `_speaker_enabled_cb_cores3`)
const AW9523_REG_P0_OUTPUT: u8 = 0x02;
const AW9523_AMP_ENABLE_BIT: u8 = 0b0000_0100;
/// 48kHz 固定 (issue #102 実機切り分けで確定)。44.1kHz だと ESP32-S3 の
/// 分数分周 (160MHz/(44.1k×256)=14+76/441) の補正が約 5.8 サイクルごとに入る
/// 高ジッタ BCK になり、AW88298 の PLL がロックできず完全無音になる
/// (SYSST.PLLS が立たない)。48kHz (13+1/48、補正が 48 サイクルに 1 回) なら
/// 安定ロックし発音する。Arduino (M5Unified) が鳴っていたのも 48kHz
const SAMPLE_RATE_HZ: u32 = 48_000;

fn aw88298_write_reg(i2c: &mut I2cDriver, reg: u8, value: u16) -> Result<()> {
    // M5Unified 同様ビッグエンディアンで書く (aw88298_write_reg の __builtin_bswap16 相当)
    i2c.write(AW88298_ADDR, &[reg, (value >> 8) as u8, value as u8], BLOCK)?;
    Ok(())
}

fn aw88298_read_reg(i2c: &mut I2cDriver, reg: u8) -> Result<u16> {
    let mut buf = [0u8; 2];
    i2c.write_read(AW88298_ADDR, &[reg], &mut buf, BLOCK)?;
    Ok(u16::from_be_bytes(buf))
}

/// AW9523 の指定ビットだけを立てる (他ビットは read-modify-write で保持)。
/// `board::power::init` が同レジスタの他ビット (BUS_EN 等) を既に書いているため
/// 絶対値書き込みではなく OR で追加する (M5Unified の `bitOn` 相当)
fn aw9523_bit_on(i2c: &mut I2cDriver, reg: u8, mask: u8) -> Result<()> {
    let mut cur = [0u8; 1];
    i2c.write_read(AW9523_ADDR, &[reg], &mut cur, BLOCK)?;
    i2c.write(AW9523_ADDR, &[reg, cur[0] | mask], BLOCK)?;
    let mut after = [0u8; 1];
    i2c.write_read(AW9523_ADDR, &[reg], &mut after, BLOCK)?;
    log::info!("speaker: AW9523 reg=0x{reg:02X} before=0x{:02X} after=0x{:02X}", cur[0], after[0]);
    Ok(())
}

/// M5Unified の rate_tbl ルックアップ (`_speaker_enabled_cb_cores3`) の移植。
/// サンプリングレートに応じてレジスタ 0x06 の下位ビットを決める
fn reg0x06_value(sample_rate_hz: u32) -> u16 {
    const RATE_TBL: [u32; 10] = [4, 5, 6, 8, 10, 11, 15, 20, 22, 44];
    let rate = (sample_rate_hz + 1102) / 2205;
    let mut idx = 0usize;
    while idx < RATE_TBL.len() - 1 && rate > RATE_TBL[idx] {
        idx += 1;
    }
    (idx as u16) | 0x14C0 // I2SBCK=0 (BCK mode 16*2)
}

/// AW88298 を I2S 入力・フル音量で有効化する (起動時に一度だけ呼ぶ)。
/// `i2c` は内部 I2C0 (main.rs で `board::power::init` に渡すのと同じハンドル) —
/// UI ループ (タッチ用) に move する前に済ませること。**`Speaker::new` +
/// `feed_silence` で BCK/WS を実際に流した後に呼ぶこと** (issue #102)。
/// 新 I2S ドライバは FIFO 空で BCK を止めるため `tx_enable()` だけでは
/// クロックが出ず、クロック無しで初期化するとアンプの PLL がロックしない
pub fn init_amp(i2c: &mut I2cDriver) -> Result<()> {
    aw9523_bit_on(i2c, AW9523_REG_P0_OUTPUT, AW9523_AMP_ENABLE_BIT)?;
    // ソフトリセット (fable diag, 2026-07-21): 何度もリフラッシュ/リセットを繰り返した
    // 影響でチップが変な内部状態にラッチしている可能性を排除する。0x04 の
    // AMPPD/PWDN とは別物 (esp_codec_dev / esp-bsp の aw88298 ドライバが init 冒頭で必ず実行)
    aw88298_write_reg(i2c, 0x00, 0x55AA)?;
    FreeRtos::delay_ms(5);
    aw88298_write_reg(i2c, 0x61, 0x0673)?; // boost mode disabled
    aw88298_write_reg(i2c, 0x04, 0x4040)?; // I2SEN=1 AMPPD=0 PWDN=0
    aw88298_write_reg(i2c, 0x05, 0x0008)?; // RMSE=0 HAGCE=0 HDCCE=0 HMUTE=0
    aw88298_write_reg(i2c, 0x06, reg0x06_value(SAMPLE_RATE_HZ))?;
    aw88298_write_reg(i2c, 0x0C, 0x0064)?; // volume: full
    // 診断: SYSST (reg 0x01) — PLL lock / クロック検出ビットが立っているか確認 (fable diag)
    match aw88298_read_reg(i2c, 0x01) {
        Ok(sysst) => log::info!("speaker: AW88298 SYSST(0x01)=0x{sysst:04X}"),
        Err(e) => log::warn!("speaker: AW88298 SYSST 読み出し失敗: {e:#}"),
    }
    // 診断: VDD (reg 0x12) — ADC 実測値から電圧を逆算 (datasheet: V=raw/1023*6.025V)。
    // UVLS フラグの真偽 (実際の電圧値) を直接確認する (2026-07-21)
    match aw88298_read_reg(i2c, 0x12) {
        Ok(raw) => {
            let mv = (raw as u32 & 0x3FF) * 6025 / 1023;
            log::info!("speaker: AW88298 VDD(0x12) raw=0x{raw:04X} ≒{mv}mV");
        }
        Err(e) => log::warn!("speaker: AW88298 VDD 読み出し失敗: {e:#}"),
    }
    Ok(())
}

/// SYSST (reg 0x01) を単発で読む (issue #102: PLL ロック安定性の診断用)
pub fn read_sysst(i2c: &mut I2cDriver) -> Result<u16> {
    aw88298_read_reg(i2c, 0x01)
}

/// AW88298 の全主要レジスタ (0x00-0x14, 0x60-0x61) をログへダンプする。
/// esp_codec_dev の aw88298_dump() と同じ範囲 (issue #102 診断)。
/// ビープ再生後に呼ぶと SYSST (0x01) の PLLS/CLKS を「クロック供給が十分
/// 安定した後」の値で読み直せる (init_amp 内の読み出しはリセット直後で
/// PLL ロック前の可能性があり、リフラッシュごとに値がばらついていた)。
/// Arduino (M5Unified) で音が出ている状態の同範囲ダンプと突き合わせ、
/// 差分レジスタを特定するのが目的
pub fn dump_regs(i2c: &mut I2cDriver) {
    for reg in (0x00u8..=0x14).chain(0x60..=0x61) {
        match aw88298_read_reg(i2c, reg) {
            Ok(v) => log::info!("speaker: AW88298[0x{reg:02X}]=0x{v:04X}"),
            Err(e) => log::warn!("speaker: AW88298[0x{reg:02X}] 読み出し失敗: {e:#}"),
        }
    }
}

pub struct Speaker {
    i2s: I2sDriver<'static, I2sTx>,
}

impl Speaker {
    /// I2S TX (I2S_NUM_1) を BCK=G34 / WS=G33 / DOUT=G13 で立てる。
    /// AW88298 の I2C 初期化 (`init_amp`) は別途、起動時に一度済ませておくこと
    pub fn new(
        i2s1: I2S1<'static>,
        bck: AnyIOPin<'static>,
        ws: AnyIOPin<'static>,
        dout: AnyIOPin<'static>,
    ) -> Result<Self> {
        let cfg = StdConfig::philips(SAMPLE_RATE_HZ, DataBitWidth::Bits16);
        let mut i2s = I2sDriver::new_std_tx(i2s1, &cfg, bck, dout, AnyIOPin::none(), ws)?;
        i2s.tx_enable()?;
        Ok(Self { i2s })
    }

    /// 無音を `duration_ms` 分流して BCK/WS を供給する (issue #102)。
    /// ESP-IDF 新 I2S ドライバは FIFO が空だと BCK を止める (TX_STOP_EN=1) ため、
    /// `tx_enable()` だけではクロックが出ない。`init_amp` の前に呼んで
    /// 「クロック供給下でアンプを初期化する」条件 (Arduino/M5Unified と同じ) を作る
    pub fn feed_silence(&mut self, duration_ms: u32) -> Result<()> {
        let n_samples = (SAMPLE_RATE_HZ * duration_ms / 1000) as usize;
        let buf = vec![0u8; n_samples * 4];
        self.i2s.write_all(&buf, BLOCK)?;
        Ok(())
    }

    /// 矩形波 (簡易ビープ音) を鳴らす。呼び出し元スレッドを `duration_ms` 分ブロックする
    /// (NFC ポーリングスレッドからの呼び出し想定 — カード検知直後の1回のみなので許容)
    pub fn beep(&mut self, freq_hz: f32, duration_ms: u32) -> Result<()> {
        let n_samples = (SAMPLE_RATE_HZ * duration_ms / 1000) as usize;
        let half_period = (SAMPLE_RATE_HZ as f32 / freq_hz / 2.0) as usize;
        let half_period = half_period.max(1);
        // 先頭 20ms は無音: FIFO 空で BCK が止まっていた場合の AW88298 PLL
        // 再ロック時間を確保する (issue #102。ロック自体は数 ms)。
        // black_box: 定数 960 (×4=3840) が畳み込まれると xtensa LLVM の
        // "Cannot select: Constant<3840>" ISel エラーでコンパイルが落ちる
        let lead_in = core::hint::black_box((SAMPLE_RATE_HZ / 50) as usize);
        // ステレオ (L/R 同値) 16bit PCM の矩形波。half_period サンプルごとに極性反転。
        // 振幅 6000 ≒ -12dB (フル音量矩形波は実機でうるさい、2026-07-21。
        // レジスタ 0x0C は 0dB のまま)。
        // 注: リードインを Vec::resize(定数長, 0) で書くと xtensa LLVM の
        // "Cannot select: Constant" ISel エラーになるため 1 ループに畳んでいる
        let mut buf = Vec::with_capacity((lead_in + n_samples) * 4);
        for n in 0..lead_in + n_samples {
            let sample: i16 = if n < lead_in {
                0
            } else if ((n - lead_in) / half_period) % 2 == 0 {
                6000
            } else {
                -6000
            };
            buf.extend_from_slice(&sample.to_le_bytes());
            buf.extend_from_slice(&sample.to_le_bytes());
        }
        self.i2s.write_all(&buf, BLOCK)?;
        Ok(())
    }
}
