//! 打刻 (`kind = "timecard"`) の組み立て: `NfcEvent` → `UplinkRecord` (issue #188)。
//!
//! atoms3-timecard の `on_card` から移設した判定部。**atoms3-timecard と CoreS3
//! の両方がここを通る** — 機種ごとに書き写さない (nfc.rs の「読み取りループを
//! 写して 3 実装目を作らないこと」と同じ)。
//!
//! 純粋な payload 組み立て (`CardKind` / `payload_json`) は
//! `alc_hub_core::timecard` にあり (host test 付き)、ここはそれに `NfcEvent`
//! (本 crate) と `UplinkRecord` (hub-common) を結ぶだけ。どちらも hub-core より
//! 下流なので hub-core には置けない (循環依存)。**hub-core と違いホストでは
//! 回らない**ので host test / coverage gate は無く、build と実機で担保する。
//!
//! # 呼び出し側に残すもの
//!
//! 音 (`Sound::PunchOk` / `PunchNg`) と `EVT TIMECARD` / `EVT NFC_MULTI_CARD` の
//! println は呼び出し側が持つ — 機種で出し方が違い (VoiceS3R は音だけ、CoreS3 は
//! 画面もある)、「`ReadFailed` で鳴らしてはいけない」「送信キューへ積めたときだけ
//! 鳴らす」(#155) といった分岐を 1 か所に隠さないため。ここは「この 1 タップを
//! 打刻にしてよいか」と「送るレコード」だけを答える。

use alc_hub_common::measurement::UplinkRecord;
use alc_hub_core::timecard::{payload_json, CardKind};

use crate::nfc::NfcEvent;

/// 打刻にできた 1 タップ。
///
/// **`card_id` は端末が読んだ生値のまま** (接頭辞を付けると punch のカード照合が
/// 必ず外れる — `alc_hub_core::timecard` の doc 参照)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Punch {
    pub card_id: String,
    pub kind: CardKind,
}

impl Punch {
    /// `NfcEvent` から打刻キーを取り出す。打刻にしないイベントは `None`:
    ///
    /// - `CarInspection`: 電子車検証は人ではない (検知ログだけ nfc.rs が出す)
    /// - `MultipleCards`: **2 枚見えたら、どちらも打刻しない** (issue #143)。
    ///   財布に 2 枚入っていると、どちらの人の打刻か決められないまま 2 人ぶん
    ///   記録してしまう — 賃金データなので曖昧なら記録しない方を採る。
    ///   **サーバへは何も送らない** (`hub_measurements` に新しい kind を足すと
    ///   rust-alc-api の HUB_MEASUREMENT_KINDS と alc-app の型・一覧まで波及する)。
    ///   ただし**黙って捨ててはいけない** (#155) — 受け手が音/表示で断ること
    /// - `ReadFailed`: かざし直せば済む。「カードが載っている間の再読が失敗した」
    ///   ときにも出る (打刻成功の直後に `rc=-6` / `rc=-4` が続く、2026-09-06) ので
    ///   受け手は**鳴らさない** (#155)
    /// - `License` で日付が 8 桁数字でない (壊れた読み取り): 16 桁のキーにできない
    ///
    /// スマホ (HCE) のランダム UID はここに来ない — nfc.rs が gate に載せる前に弾く
    /// (#155、`alc_hub_core::nfca_uid`)。実タグ (7B の NTAG / MIFARE) はそのまま打刻
    pub fn from_event(event: &NfcEvent) -> Option<Self> {
        let (card_id, kind) = match event {
            NfcEvent::Felica { idm } => (idm.clone(), CardKind::FelicaIdm),
            NfcEvent::NfcaUid { uid } => (uid.clone(), CardKind::NfcaUid),
            // 免許証は「交付日 8 桁 + 有効期限 8 桁」= alc-app タブレットが使う
            // employees.nfc_id と同じキー。punch はカード未登録なら
            // employees.nfc_id へフォールバックするので、この 16 桁で当たる
            NfcEvent::License { issue, expiry } => {
                let card = alc_hub_core::tenko_prompt::LicenseCard {
                    issue: issue.clone(),
                    expiry: expiry.clone(),
                };
                match card.nfc_id() {
                    Some(id) => (id, CardKind::License),
                    None => {
                        log::warn!("timecard: 免許証の日付が想定外 issue={issue} expiry={expiry}");
                        return None;
                    }
                }
            }
            NfcEvent::CarInspection { .. }
            | NfcEvent::MultipleCards
            | NfcEvent::ReadFailed { .. } => return None,
        };
        Some(Punch { card_id, kind })
    }

    /// 送信キューへ積む 1 レコード。`session_id` は点呼ではないので付けない。
    ///
    /// `recorded_at_ms` は打刻時刻 (epoch ms)。NTP 未同期なら ws_uplink が送信時に
    /// `at_ms` (稼働時間) との差で補正する
    pub fn record(&self, at_ms: u64, recorded_at_ms: u64) -> UplinkRecord {
        UplinkRecord {
            kind: "timecard",
            payload: payload_json(&self.card_id, self.kind),
            recorded_at_ms,
            at_ms,
            session_id: None,
        }
    }
}

/// `NfcEvent` → 送るレコード。打刻にしないイベントは `None` ([`Punch::from_event`])。
/// `card_id` / 種別をログに出したい呼び出し側は `Punch` を直接使う
pub fn punch_record(event: &NfcEvent, at_ms: u64, recorded_at_ms: u64) -> Option<UplinkRecord> {
    Punch::from_event(event).map(|p| p.record(at_ms, recorded_at_ms))
}
