//! 対象 BLE 機器 (ニプロ体温計/血圧計、Omron 血圧計) の種別と判定。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Thermometer,
    BloodPressure,
}

impl DeviceKind {
    /// ble-medical-gateway のシリアル JSON 互換のデバイス名
    pub fn json_name(self) -> &'static str {
        match self {
            Self::Thermometer => "thermometer",
            Self::BloodPressure => "blood_pressure",
        }
    }

    /// 画面・イベントログ用の日本語名
    pub fn jp_name(self) -> &'static str {
        match self {
            Self::Thermometer => "体温計",
            Self::BloodPressure => "血圧計",
        }
    }
}

/// Omron の BLE 機器 (HEM-6231T / HCR-1901T2) の広告がどちらの状態か。
/// HEM-6231T は service UUID を広告せず名前で見分ける (Refs #237、Linux で実測)。
/// HCR-1901T2 は本体広告のメーカーデータで見分ける ([`omron_adv_from_mfg`])。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OmronAdv {
    /// ペアリング待ち (機器の -P- 点滅中)。unlock 鍵の登録に接続する
    Pairing,
    /// 測定後の送信。bond 済みで接続し 0x2A35 を受け取る
    Transfer,
}

/// 広告名から Omron 機器の状態を返す。接頭辞の大文字小文字で状態が変わる
/// (ペアリング待ち `BLEsmart_`、送信 `BLESmart_`) ので区別して比べる。
///
/// **HCR-1901T2 には効かない** — ペアリング待ちでも `BLESmart_` のままなので、
/// 本体広告のメーカーデータを見る [`omron_adv_from_mfg`] を先に当てる。
pub fn omron_adv(name: &str) -> Option<OmronAdv> {
    if name.starts_with("BLEsmart_") {
        Some(OmronAdv::Pairing)
    } else if name.starts_with("BLESmart_") {
        Some(OmronAdv::Transfer)
    } else {
        None
    }
}

/// HCR-1901T2 のメーカーデータ (company `0x020E` の payload) の先頭バイト。
/// 実測した payload は `06 <flags> <n1> 00 <n2> 00×7` の 12 byte
const OMRON_MFG_HEAD: u8 = 0x06;
const OMRON_MFG_LEN: usize = 12;
/// `flags` のこのビットが立っている間だけ機器はペアリング待ち (`-P-` 点滅)
const OMRON_MFG_PAIRING_BIT: u8 = 0x08;

/// Omron 機の**接続可能な本体広告**のメーカーデータから状態を返す。
///
/// HCR-1901T2 は名前が scan response にしか載らず、しかも**ペアリング待ちでも
/// `BLESmart_` (大文字 S) のまま**で [`omron_adv`] の大文字小文字判定が効かない
/// (Windows で実測)。状態を持つのはメーカーデータの `flags` の bit3 だけで、
/// `0x29` = `-P-` 点滅 / `0x21` = 通常。同じ payload の `n1` / `n2` は測定の
/// 累積件数で、送信済みになっても減らない (= 未送信件数ではない) ので使わない。
///
/// 形 (12 byte・先頭 `0x06`) が違う機種 (HEM-6231T など) は `None` を返し、
/// 従来どおり [`omron_adv`] の名前判定に任せる。
#[must_use]
pub fn omron_adv_from_mfg(payload: &[u8]) -> Option<OmronAdv> {
    if payload.len() != OMRON_MFG_LEN || payload[0] != OMRON_MFG_HEAD {
        return None;
    }
    Some(if payload[1] & OMRON_MFG_PAIRING_BIT == 0 {
        OmronAdv::Transfer
    } else {
        OmronAdv::Pairing
    })
}

/// Omron 機の広告名 (`BLESmart_000000A1F8B3719433A1`) から機器の MAC を起こす。
///
/// ペアリング待ちの広告は S3R の NimBLE ではアドレスが全 0 で届く (同時刻の Windows は
/// `F8:B3:71:94:33:A1` を受けているので、機器は実アドレスで広告している)。接続先が無いと
/// ペアリングできないので、名前の末尾 12 桁 (= MAC の big endian 表記) から起こす。
/// 前半の 8 桁は機種コードで、機器ごとに変わらない
#[must_use]
pub fn omron_addr_from_name(name: &str) -> Option<[u8; 6]> {
    let hex = name
        .strip_prefix("BLESmart_")
        .or_else(|| name.strip_prefix("BLEsmart_"))?;
    if hex.len() < 12 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mac = &hex[hex.len() - 12..];
    let mut out = [0u8; 6];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(mac.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// アドバタイズのデバイス名から種別を判定する (Arduino 版の名前判定を移植。
/// ニプロ機器が標準サービス UUID を広告しない場合の対策)。
/// Omron 機器 (独自名) も血圧計として拾う。
pub fn match_device_name(name: &str) -> Option<DeviceKind> {
    if omron_adv(name).is_some() {
        return Some(DeviceKind::BloodPressure);
    }
    if name.contains("NT-100") || name.contains("Thermo") {
        return Some(DeviceKind::Thermometer);
    }
    if name.contains("NBP-1") || name.contains("BP") || name.contains("Blood") {
        return Some(DeviceKind::BloodPressure);
    }
    None
}

/// 血圧計がボンドされているか (Refs #249)。
///
/// `recorded` は**ペアリング成功時に NVS へ書いた血圧計のアドレス** (無ければ
/// `None`)、`bonded` は NimBLE が今持っているボンド一覧のアドレス。
/// **真偽そのものは永続化しない** — 記録したアドレスがボンド一覧にまだ居るか
/// で毎回決めるので、`PAIR` でのボンド消去や機器側の解除と自動で整合する。
///
/// アドレスは NimBLE の native 表現 (little endian の 6 B) で比べる。
/// `BLEAddress` の `==` も 6 B だけを見るので、型 (public/random) は無視してよい。
#[must_use]
pub fn bp_bonded(recorded: Option<[u8; 6]>, bonded: &[[u8; 6]]) -> bool {
    recorded.is_some_and(|addr| bonded.contains(&addr))
}

/// `HubStatus` から読んだ血圧計の観測 (Refs #269)。
///
/// **bool を引数に並べない** — 引数が増えると呼び出し側が順序を取り違えても
/// 型検査が捕まえられない ([`should_remember_bp_bond`] と同じ理由)。
/// フィールド名で渡せば取り違えはコンパイルエラーになる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BpObservation {
    /// BLE central (hub-ble) がこの機で動いているか (`HubStatus::ble_running`)
    pub ble_running: bool,
    /// BLE スキャンが一度でも回って `bonded` を書いたか (`HubStatus::bp_read`)
    pub read: bool,
    /// そのスキャンが書いたボンド状態 (`HubStatus::bp_bonded`)
    pub bonded: bool,
}

/// 血圧計のボンド状態として**報告してよい値** (Refs #269)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BpReport {
    /// **まだ確認できていない。**「未ボンド」ではないので、`bp=0` の顔をして
    /// 外へ出してはいけない
    NotReady,
    /// スキャンが回って確定した観測
    Ready {
        /// 血圧計がボンドされているか ([`bp_bonded`] の結果)
        bonded: bool,
    },
}

/// 観測から「報告してよい値」を決める (Refs #269)。
///
/// `observed` が `None` なのは**状態を読めなかったとき** (`HubStatus` の lock
/// 失敗) で、未スキャンと同じ「未確認」に倒す。
///
/// # 真理値表
///
/// | `ble_running` | `read` | 返す値 | なぜ |
/// |---|---|---|---|
/// | false | — | `Ready { bonded: false }` | BLE を起こさない機 (警告デバイス / 印刷ブリッジ / Lite build / `OMRON BP OFF`)。既定値の false が**確定した「血圧計なし」** — ここで待つと永久に答えられない |
/// | true | false | `NotReady` | スキャン前。既定値の false は観測結果ではない |
/// | true | true | `Ready { bonded }` | 実測 |
///
/// # なぜゲートが要るか
///
/// `HubStatus::bp_bonded` の既定値は `false` で、hub-ble のスキャンが 1 周して
/// 初めて実測が入る (`bp_read` が立つのが同じ瞬間)。ホスト (PWA) が
/// `port.open()` するとチップがリセットされるため、**問い合わせはほぼ毎回
/// 「スキャン前」の窓に当たる**。そこで `bp_bonded` を生で読むと、血圧計が
/// 繋がっている端末が `bp=0` を名乗る — 法定の血圧記録を省く側へ倒れてしまう
/// (`should_remember_bp_bond` の doc と同じ危険)。
///
/// 判定を**この関数 1 本**に集める。`AUTH SIGNBP` の署名 (hub-drivers の
/// console.rs) と `/device/setup` の `bp_status` 照会 (ws_uplink.rs) が別々に
/// 書くと、`#249` → `#252` → `#253` → `#266` → `#269` と同じ食い違いを繰り返す。
#[must_use]
pub fn bp_report(observed: Option<BpObservation>) -> BpReport {
    match observed {
        Some(BpObservation {
            ble_running: false, ..
        }) => BpReport::Ready { bonded: false },
        Some(BpObservation {
            read: true, bonded, ..
        }) => BpReport::Ready { bonded },
        Some(BpObservation { read: false, .. }) | None => BpReport::NotReady,
    }
}

/// 血圧計のボンド記録を書いてよい瞬間 (Refs #266)。
///
/// hub-ble がこの判定を呼ぶ 3 点を、そのまま variant にしてある。
/// **bool を引数に並べない** — 引数が増えると呼び出し側が順序を取り違えても型検査が
/// 捕まえられず、それが `#249` → `#252` → `#253` → `#266` と同じ穴を 4 世代
/// 繰り返した原因。観測点を名前で渡せば、取り違えはコンパイルエラーになる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BpBondSite {
    /// 送信広告を出している Omron 機が、既に NimBLE のボンド一覧に居た。
    /// このファームより前にペアリングを済ませていた機 (OTA で上がってきた現場) が
    /// 記録を持たないので、ここで書き足す
    OmronBondSeen,
    /// Omron のペアリングが成功した (`EVT PAIR_OK` を出すのと同じ瞬間)
    OmronPaired,
    /// 1 回の接続が終わった (Omron / 非 Omron 共通の後始末)
    ConnectionFinished {
        /// 広告名から確定した機器種別
        kind: DeviceKind,
        /// Omron 機の広告状態 (非 Omron なら `None`)
        omron: Option<OmronAdv>,
        /// その接続で測定を実際に受け取れたか
        got_data: bool,
    },
}

/// この観測点で「血圧計としてボンドした」と記録してよいか (Refs #249 / #252 / #266)。
///
/// 記録は [`bp_bonded`] の片側 — つまり `AUTH SIGNBP` が署名する `bp=1` の根拠になる。
/// 判定は**この関数だけ**が持つ。以前は hub-ble 側の 3 か所に別々の条件で散っており、
/// うち 2 つはこの述語を迂回していた。その結果「`EVT PAIR_OK` を出したのに `BP=0`」
/// という状態が作れてしまい、現場が止まった (#266)。
///
/// # 真理値表
///
/// | 観測点 | 記録する | なぜ |
/// |---|---|---|
/// | [`BpBondSite::OmronBondSeen`] | ✔ | ボンド一覧に居る Omron 機 = 血圧計が在る観測 |
/// | [`BpBondSite::OmronPaired`] | ✔ | `EVT PAIR_OK` と同時に書く。成功表示と記録を一致させる |
/// | `ConnectionFinished` 血圧計・非 Omron・測定あり | ✔ | 血圧の特性を実際に読めた |
/// | `ConnectionFinished` 血圧計・非 Omron・測定なし | ✘ | 名前判定が緩く、無関係な機器を載せうる |
/// | `ConnectionFinished` 体温計 (測定の有無によらず) | ✘ | 血圧計ではない |
/// | `ConnectionFinished` Omron 機 (測定の有無によらず) | ✘ | 上 2 つが記録済み。二重に書かない |
///
/// # Omron 経路を常に `true` に倒してある理由
///
/// Omron の 2 点は血圧計である根拠が広告 (company id `0x020E` / 広告名) だけで、
/// legacy HEM-6231T は購読も `0x1810` の確認もせずペアリング成功を返す。それでも
/// **`bp=0` に倒す方が危険**で、記録が無いまま `bp=0` を署名すると血圧計が在るのに
/// 法定の血圧記録を省く側へ倒れてしまう。誤って `bp=1` になった場合 (血圧計でない
/// Omron 機を `PAIR` した場合) は次の測定で即座に露見し、しかも `PAIR` は現場の人が
/// 血圧計を目の前にして押す操作なので、**誤ペアリングは検知ではなく操作で防ぐ**。
#[must_use]
pub fn should_remember_bp_bond(site: BpBondSite) -> bool {
    match site {
        BpBondSite::OmronBondSeen | BpBondSite::OmronPaired => true,
        BpBondSite::ConnectionFinished {
            kind,
            omron,
            got_data,
        } => kind == DeviceKind::BloodPressure && omron.is_none() && got_data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BP: [u8; 6] = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06];
    const OTHER: [u8; 6] = [0x11, 0x12, 0x13, 0x14, 0x15, 0x16];

    #[test]
    fn bp_bonded_needs_both_record_and_live_bond() {
        // 記録が在り、ボンド一覧にも居る = ボンドされている
        assert!(bp_bonded(Some(BP), &[OTHER, BP]));
        // 記録が無い (一度もペアリングしていない)
        assert!(!bp_bonded(None, &[BP]));
        // 記録は在るがボンドは消えている (PAIR での全消去・機器側の解除)
        assert!(!bp_bonded(Some(BP), &[OTHER]));
        assert!(!bp_bonded(Some(BP), &[]));
        // 体温計だけがボンドされていても血圧計にはならない
        assert!(!bp_bonded(None, &[OTHER]));
    }

    /// `ConnectionFinished` を組み立てる (真理値表の 4 行目以降)。
    ///
    /// enum 化する前の `should_remember_bp_bond(kind, omron, got_data)` は
    /// hub-ble の `:404` 専用だったので、**旧テストの 6 ケースはすべてこの variant**
    /// に移した。下の `remembers_bp_bond_only_when_bp_data_arrived` の ① 〜 ⑥ が
    /// その 6 つで、入力も結論も変えていない (⑦ ⑧ は今回足した網羅ぶん)
    fn finished(kind: DeviceKind, omron: Option<OmronAdv>, got_data: bool) -> BpBondSite {
        BpBondSite::ConnectionFinished {
            kind,
            omron,
            got_data,
        }
    }

    /// `BpObservation` を組み立てる (BLE が動いている機の観測)
    fn observed(read: bool, bonded: bool) -> Option<BpObservation> {
        Some(BpObservation {
            ble_running: true,
            read,
            bonded,
        })
    }

    /// #269: スキャンが 1 周するまでは「未ボンド」と答えない。
    /// 起動直後の既定値 `false` を `bp=0` として署名すると、血圧計が在る端末が
    /// 無いと名乗る
    #[test]
    fn bp_report_withholds_bond_state_until_the_scan_ran() {
        assert_eq!(bp_report(observed(false, false)), BpReport::NotReady);
        // 未読なら `bonded` が何であれ報告しない (読み取り順の取り違え検知)
        assert_eq!(bp_report(observed(false, true)), BpReport::NotReady);
        // 状態そのものを読めなかった (lock 失敗) ときも未確認
        assert_eq!(bp_report(None), BpReport::NotReady);
    }

    #[test]
    fn bp_report_passes_the_observation_through_once_the_scan_ran() {
        assert_eq!(
            bp_report(observed(true, false)),
            BpReport::Ready { bonded: false }
        );
        assert_eq!(
            bp_report(observed(true, true)),
            BpReport::Ready { bonded: true }
        );
    }

    /// BLE を起こさない機 (警告デバイス / 印刷ブリッジ / Lite build /
    /// `OMRON BP OFF`) は**待たせない** — `bp_read` は永久に立たないので、
    /// 待つと `AUTH SIGNBP` が一生答えられなくなる。既定値の false が
    /// そのまま確定した「血圧計なし」
    #[test]
    fn bp_report_is_final_when_ble_never_runs() {
        assert_eq!(
            bp_report(Some(BpObservation {
                ble_running: false,
                read: false,
                bonded: false,
            })),
            BpReport::Ready { bonded: false }
        );
    }

    #[test]
    fn remembers_bp_bond_when_omron_pairing_succeeds() {
        // #266: EVT PAIR_OK を出した回は必ず記録する。測定を 1 件も受け取れなくても
        // (EVT OMRON_END by=peer n=0)、成功表示と記録が食い違ってはいけない
        assert!(should_remember_bp_bond(BpBondSite::OmronPaired));
    }

    #[test]
    fn remembers_bp_bond_when_bonded_omron_seen() {
        // 既にボンド一覧に居る Omron 機を見た = 血圧計が在る観測点 (Refs #249)
        assert!(should_remember_bp_bond(BpBondSite::OmronBondSeen));
    }

    #[test]
    fn remembers_bp_bond_only_when_bp_data_arrived() {
        // ① 血圧計から測定を実際に受け取れた = 「血圧計としてボンドした」と記録する
        assert!(should_remember_bp_bond(finished(
            DeviceKind::BloodPressure,
            None,
            true
        )));
        // ② 接続はできたがデータなし: 記録しない。名前判定が緩いので、無関係な
        // 機器を載せると bp=1 を署名してしまう
        assert!(!should_remember_bp_bond(finished(
            DeviceKind::BloodPressure,
            None,
            false
        )));
        // ③ ④ 体温計は測定を受け取れても血圧のボンド記録を書かない
        assert!(!should_remember_bp_bond(finished(
            DeviceKind::Thermometer,
            None,
            true
        )));
        assert!(!should_remember_bp_bond(finished(
            DeviceKind::Thermometer,
            None,
            false
        )));
        // ⑤ ⑥ Omron は Pairing / Transfer とも専用の観測点が記録済み — 二重に書かない。
        // ⑦ ⑧ (got_data = false) は旧テストに無かったぶん — 測定の有無によらず
        // false であることを固定する
        assert!(!should_remember_bp_bond(finished(
            DeviceKind::BloodPressure,
            Some(OmronAdv::Pairing),
            true
        )));
        assert!(!should_remember_bp_bond(finished(
            DeviceKind::BloodPressure,
            Some(OmronAdv::Transfer),
            true
        )));
        assert!(!should_remember_bp_bond(finished(
            DeviceKind::BloodPressure,
            Some(OmronAdv::Pairing),
            false
        )));
        assert!(!should_remember_bp_bond(finished(
            DeviceKind::BloodPressure,
            Some(OmronAdv::Transfer),
            false
        )));
    }

    #[test]
    fn json_names() {
        assert_eq!(DeviceKind::Thermometer.json_name(), "thermometer");
        assert_eq!(DeviceKind::BloodPressure.json_name(), "blood_pressure");
    }

    #[test]
    fn jp_names() {
        assert_eq!(DeviceKind::Thermometer.jp_name(), "体温計");
        assert_eq!(DeviceKind::BloodPressure.jp_name(), "血圧計");
    }

    #[test]
    fn matches_thermometer_names() {
        assert_eq!(match_device_name("NT-100B"), Some(DeviceKind::Thermometer));
        assert_eq!(
            match_device_name("MyThermometer"),
            Some(DeviceKind::Thermometer)
        );
    }

    #[test]
    fn matches_blood_pressure_names() {
        assert_eq!(
            match_device_name("NBP-1BLE"),
            Some(DeviceKind::BloodPressure)
        );
        assert_eq!(match_device_name("BP-Meter"), Some(DeviceKind::BloodPressure));
        assert_eq!(
            match_device_name("BloodPressure"),
            Some(DeviceKind::BloodPressure)
        );
    }

    #[test]
    fn unknown_name() {
        assert_eq!(match_device_name("FC-1200"), None);
    }

    #[test]
    fn omron_adv_states() {
        assert_eq!(omron_adv("BLEsmart_test"), Some(OmronAdv::Pairing));
        assert_eq!(omron_adv("BLESmart_test"), Some(OmronAdv::Transfer));
        assert_eq!(omron_adv("blesmart_test"), None);
        assert_eq!(omron_adv("NBP-1BLE"), None);
    }

    /// HCR-1901T2 の本体広告 (company 0x020E の payload) を実測値で当てる
    #[test]
    fn omron_adv_from_mfg_states() {
        // -P- 点滅中 (flags bit3 が立つ)
        let pairing = [
            0x06, 0x29, 0x03, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        assert_eq!(omron_adv_from_mfg(&pairing), Some(OmronAdv::Pairing));
        // 通常 (測定の累積件数 n1/n2 が進んでも状態は変わらない)
        let transfer = [
            0x06, 0x21, 0x04, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        assert_eq!(omron_adv_from_mfg(&transfer), Some(OmronAdv::Transfer));
    }

    /// 形が違うメーカーデータ (別機種・iBeacon 等) は名前判定に任せる
    #[test]
    fn omron_adv_from_mfg_ignores_other_shapes() {
        // 長さ違い
        assert_eq!(omron_adv_from_mfg(&[0x06, 0x21]), None);
        assert_eq!(omron_adv_from_mfg(&[]), None);
        // 先頭バイト違い
        let head = [
            0x02, 0x21, 0x04, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        assert_eq!(omron_adv_from_mfg(&head), None);
    }

    /// 広告名の末尾 12 桁が MAC (big endian)
    #[test]
    fn omron_addr_from_name_parses_mac() {
        assert_eq!(
            omron_addr_from_name("BLESmart_000000A1F8B3719433A1"),
            Some([0xF8, 0xB3, 0x71, 0x94, 0x33, 0xA1])
        );
        // ペアリング待ちの小文字名でも同じ
        assert_eq!(
            omron_addr_from_name("BLEsmart_000000A1F8B3719433A1"),
            Some([0xF8, 0xB3, 0x71, 0x94, 0x33, 0xA1])
        );
    }

    #[test]
    fn omron_addr_from_name_rejects_other_names() {
        // 接頭辞違い
        assert_eq!(omron_addr_from_name("NBP-1BLE"), None);
        // 桁が足りない
        assert_eq!(omron_addr_from_name("BLESmart_A1F8B3"), None);
        // 16 進でない文字が混じる
        assert_eq!(omron_addr_from_name("BLESmart_000000A1F8B37194ZZA1"), None);
    }

    #[test]
    fn matches_omron_names() {
        assert_eq!(
            match_device_name("BLEsmart_test"),
            Some(DeviceKind::BloodPressure)
        );
        assert_eq!(
            match_device_name("BLESmart_test"),
            Some(DeviceKind::BloodPressure)
        );
    }
}
