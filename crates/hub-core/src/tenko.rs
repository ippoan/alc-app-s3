//! 点呼の構成項目と完了条件・段レイアウト (純粋ロジック)。
//!
//! 点呼は **体温 + アルコール** が基本で、**血圧は運用オプション** (既定 OFF、
//! `TENKO BP ON|OFF` で NVS に保存)。血圧計を置かない拠点では画面に血圧の段を
//! 出さず、体温とアルコールの 2 段で大きく表示する。完了条件も構成に従う
//! (必須項目がすべて揃ったら TENKO_DONE_CLOSE_MS 後に待機画面へ)。
//!
//! 血圧 OFF のときに血圧計で測っても、測定値はログ・WS 送信には残る (recorder は
//! 構成を見ない)。画面だけが無視する。

/// 点呼の構成 (どの項目を必須にするか)。体温・アルコールは常に必須
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TenkoItems {
    /// 血圧を点呼に含める (既定 false = 保留)
    pub bp: bool,
}

/// 点呼画面の段 (上から順)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TenkoRow {
    Temp,
    Bp,
    Alcohol,
}

const ROWS_WITH_BP: [TenkoRow; 3] = [TenkoRow::Temp, TenkoRow::Bp, TenkoRow::Alcohol];
const ROWS_WITHOUT_BP: [TenkoRow; 2] = [TenkoRow::Temp, TenkoRow::Alcohol];

impl TenkoItems {
    /// 画面に出す段 (上から順)
    pub fn rows(self) -> &'static [TenkoRow] {
        if self.bp {
            &ROWS_WITH_BP
        } else {
            &ROWS_WITHOUT_BP
        }
    }

    /// 必須項目がすべて揃ったか (= 点呼完了)。血圧は構成に含むときだけ見る
    pub fn complete(self, temp: bool, bp: bool, alcohol: bool) -> bool {
        temp && alcohol && (!self.bp || bp)
    }

    /// `row` の縦の位置 (y, 高さ)。`top`..`bottom` を段数で等分する。
    /// 構成に無い段 (血圧 OFF のときの Bp) は None
    pub fn row_span(self, row: TenkoRow, top: i32, bottom: i32) -> Option<(i32, i32)> {
        let rows = self.rows();
        let idx = rows.iter().position(|r| *r == row)?;
        let h = (bottom - top) / rows.len() as i32;
        Some((top + h * idx as i32, h))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_without_bp() {
        assert_eq!(TenkoItems::default(), TenkoItems { bp: false });
        assert_eq!(
            TenkoItems::default().rows(),
            &[TenkoRow::Temp, TenkoRow::Alcohol]
        );
        assert_eq!(
            TenkoItems { bp: true }.rows(),
            &[TenkoRow::Temp, TenkoRow::Bp, TenkoRow::Alcohol]
        );
    }

    #[test]
    fn complete_without_bp_needs_temp_and_alcohol() {
        let it = TenkoItems { bp: false };
        assert!(!it.complete(false, false, false));
        assert!(!it.complete(true, false, false));
        assert!(!it.complete(false, false, true));
        assert!(it.complete(true, false, true));
        // 血圧が来ても関係ない
        assert!(it.complete(true, true, true));
        assert!(!it.complete(false, true, true));
    }

    #[test]
    fn complete_with_bp_needs_all_three() {
        let it = TenkoItems { bp: true };
        assert!(!it.complete(true, false, true));
        assert!(!it.complete(true, true, false));
        assert!(it.complete(true, true, true));
    }

    #[test]
    fn row_span_two_rows() {
        // 320x240 横向き: バー 18px → 222px を 2 等分 (111px)
        let it = TenkoItems { bp: false };
        assert_eq!(it.row_span(TenkoRow::Temp, 18, 240), Some((18, 111)));
        assert_eq!(it.row_span(TenkoRow::Alcohol, 18, 240), Some((129, 111)));
        assert_eq!(it.row_span(TenkoRow::Bp, 18, 240), None);
    }

    #[test]
    fn row_span_three_rows() {
        // 従来の 3 段 (74px) と一致
        let it = TenkoItems { bp: true };
        assert_eq!(it.row_span(TenkoRow::Temp, 18, 240), Some((18, 74)));
        assert_eq!(it.row_span(TenkoRow::Bp, 18, 240), Some((92, 74)));
        assert_eq!(it.row_span(TenkoRow::Alcohol, 18, 240), Some((166, 74)));
    }
}
