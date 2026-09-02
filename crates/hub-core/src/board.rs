//! ボード種別 (CoreS3 / CoreS3 SE) の判定 (純粋ロジック)。
//!
//! CoreS3 SE は CoreS3 から **カメラ / IMU (BMI270) / 地磁気 (BMM150) / RTC
//! (BM8563) / 近接 (LTR-553) / 内蔵バッテリー** を削った廉価版で、ESP32-S3 /
//! LCD / タッチ / AXP2101 / AW9523 / スピーカーは共通。firmware が使うのは共通
//! 部分だけなので同じバイナリがそのまま動くが、
//!
//! - バッテリーが無いので Log 画面の "bat0% 放電" は誤情報になる
//! - 不具合報告のとき「どっちの板か」を STATUS で機械的に確認したい
//!
//! ため、起動時に内部 I2C を probe して種別を持つ。判定は hub-board 側の
//! I2C ack (`hub_board::board::probe`) の結果をここに渡して決める
//! (plan/cores3-hub-consolidation.md「次期構成: CoreS3 SE + Base LAN PoE v1.2」)。

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BoardKind {
    /// 起動直後で未判定
    #[default]
    Unknown,
    /// M5Stack CoreS3 (フル構成)
    CoreS3,
    /// M5Stack CoreS3 SE (IMU / RTC / バッテリー無し)
    CoreS3Se,
}

impl BoardKind {
    /// 内部 I2C の probe 結果から判定する。SE には RTC (0x51) も IMU (0x69) も
    /// 載っていない。片方でも ack すればフル構成の CoreS3 とみなす
    /// (I2C の一時的な NAK 1 回で SE と誤判定しないよう、両方欠けて初めて SE)
    pub fn from_probe(rtc_present: bool, imu_present: bool) -> Self {
        if rtc_present || imu_present {
            Self::CoreS3
        } else {
            Self::CoreS3Se
        }
    }

    /// `STATUS BOARD=` に載せる機械可読ラベル
    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::CoreS3 => "cores3",
            Self::CoreS3Se => "cores3se",
        }
    }

    /// 画面 (Log) 用の短い表示名
    pub fn short_name(self) -> &'static str {
        match self {
            Self::Unknown => "?",
            Self::CoreS3 => "CoreS3",
            Self::CoreS3Se => "CoreS3 SE",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_unknown() {
        assert_eq!(BoardKind::default(), BoardKind::Unknown);
    }

    #[test]
    fn probe_decides_kind() {
        assert_eq!(BoardKind::from_probe(true, true), BoardKind::CoreS3);
        assert_eq!(BoardKind::from_probe(true, false), BoardKind::CoreS3);
        assert_eq!(BoardKind::from_probe(false, true), BoardKind::CoreS3);
        assert_eq!(BoardKind::from_probe(false, false), BoardKind::CoreS3Se);
    }

    #[test]
    fn labels() {
        assert_eq!(BoardKind::Unknown.label(), "unknown");
        assert_eq!(BoardKind::CoreS3.label(), "cores3");
        assert_eq!(BoardKind::CoreS3Se.label(), "cores3se");
        assert_eq!(BoardKind::Unknown.short_name(), "?");
        assert_eq!(BoardKind::CoreS3.short_name(), "CoreS3");
        assert_eq!(BoardKind::CoreS3Se.short_name(), "CoreS3 SE");
    }
}
