//! ホストリンク行プロトコルの解析 (純粋部分)。
//!
//! I/O・画面遷移・NVS 保存などの副作用は firmware 側 (host_link.rs) が担い、
//! ここでは「1 行 → コマンド or エラー応答文字列」の変換のみを行う。

/// M-Bus 5V を Core 側から出すか (`BUS5V AUTO|ON|OFF`。NVS 保存、既定 `Auto`)。
///
/// #200/#201 で入れたこの設定は #203 で一度廃止したが、**PoE の現場は `OFF` に
/// して運用していた**。読む側だけが消えて NVS の値が無視され、固定動作に
/// 置き換わった結果、現場の端末が PoE 単独給電で起動しなくなった (#254)。
/// 設定を戻し、**既定は #203 以降の固定動作 (= `Auto`) のまま**にする。
///
/// - `Auto` (既定): #203 以降の固定動作。**USB ホスト (PC) が列挙されていて、
///   かつ M-Bus が外部給電でない (`HubStatus::bus_in == Some(false)`) ときだけ**
///   出す。PC が居るなら VBUS がレールを支え、PC が落ちれば Core は手を引く
/// - `On`: 常に出す。**USB 電源アダプタ**で動かすベンチ (PC ではないので USB
///   ホストとして列挙されず `Auto` では出ない) に RS232M / LAN 13.2 を積む構成
///   向け (Refs #76)。★**PoE のベースを履いた機では使わないこと** — 同じ 5V
///   レールを両側から駆動することになり、バッテリーで突入を吸収できない
///   CoreS3 SE は PoE 単独給電で起動できなくなる。#203 は実機で
///   「`on` は PC 再起動中に PoE 単独で起動できない」を確認している
/// - `Off`: 常に出さない。**ベース側 (PoE) から給電する常設機はこれ** (#254)
///
/// 判定そのものは [`crate::usb5v::bus5v_sample`] に置く (hub-ui が呼ぶ)。
///
/// ★**NVS の保存値 (u8) は #203 以前と同じ対応を保つこと** — 現場が設定した値が
/// NVS (`bus5v` キー) にまだ残っているため、対応を変えると読み違える
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Bus5vMode {
    #[default]
    Auto,
    On,
    Off,
}

impl Bus5vMode {
    /// NVS 保存値 (u8) から復元する。未知の値は既定 (`Auto`)
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::On,
            2 => Self::Off,
            _ => Self::Auto,
        }
    }

    /// NVS 保存値 (u8)
    pub fn to_u8(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::On => 1,
            Self::Off => 2,
        }
    }

    /// 応答・ログ用のラベル (`BUS5V MODE=auto`)
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::On => "on",
            Self::Off => "off",
        }
    }
}

/// このホストコンソールを持つ機種 (Refs ippoan/alc-app#353)。
///
/// `DEVICE` 応答の名乗り (`DEVICE <label> VER=…`) と `ERR UNSUPPORTED (<label>)`
/// の両方をここ 1 か所から出す — 別々の文字列がそれぞれの口に散っていたのが
/// 「新しい機種が名乗りを書き忘れる」穴の根だった。
///
/// `label` は auth-worker の `DEVICE_KINDS` の key に揃えてある。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKind {
    /// M5Stack CoreS3 / CoreS3 SE (運行者タブ、シリアル署名認証)
    CoreS3,
    /// AtomS3 印刷ブリッジ
    AtomS3Print,
    /// NFC タイムカード端末
    Timecard,
    /// 警告デバイス (VoiceS3R)
    Alarm,
    /// 血圧計用 PC の測定台 (`atoms3-nfc` の VoiceS3R build)
    BpStation,
}

impl HostKind {
    /// `DEVICE <label> …` の 2 トークン目、`ERR UNSUPPORTED (<label>)` に使う
    /// 機械可読ラベル (auth-worker の `DEVICE_KINDS` の key と同じ語彙)
    pub fn label(self) -> &'static str {
        match self {
            Self::CoreS3 => "cores3",
            Self::AtomS3Print => "atoms3-print",
            Self::Timecard => "timecard",
            Self::Alarm => "alarm",
            Self::BpStation => "bp-station",
        }
    }

    /// `AUTH TICKET` (ippoan/alc-app-s3#204) を出してよいか。この券は USB で
    /// 繋がった**運行者 PWA ブラウザ**への受け渡しが前提で、それが繋がるのは
    /// CoreS3 だけ (`alc-hub-drivers::console::handle_common` 参照)
    pub fn claim_ticket(self) -> bool {
        matches!(self, Self::CoreS3)
    }

    /// `DEVICE` 応答に `BOARD=<label>` を足すか。板種 (CoreS3 / CoreS3 SE) が
    /// 複数あるのは今のところ CoreS3 だけ ([`crate::board::BoardKind`])。
    /// 元は `STATUS` に載っていたが、板種は不変なので名乗りの側が筋
    /// (Refs ippoan/alc-app#353)
    pub fn has_board(self) -> bool {
        matches!(self, Self::CoreS3)
    }
}

/// ホスト (Windows PC / Android タブレット) からのコマンド
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostCommand {
    Ping,
    ShowQr { payload: String, timeout_ms: u64 },
    Measure,
    Result { ok: bool, value: String },
    ShowError { message: String },
    Reset,
    /// PC (運行者タブ) の点呼の今の段 (`OK STAGE <label>` を応答)。点呼画面の
    /// どの欄を強調するか・PC だけで進む段かを切り替える。結果は従来の `RESULT`
    Stage(HostStage),
    Rotate(u16),
    Status,
    /// 機種の名乗り (`DEVICE <kind> VER=<version>` を応答)。**全機種が
    /// `handle_common` で共通に答える** — `STATUS` と違い機種ごとの実装を
    /// 持たない (Refs ippoan/alc-app#353)
    Device,
    /// 設定のエクスポート (`CFG <json>` を応答)
    CfgGet,
    /// 設定のインポート (JSON は cfg::DeviceConfig::from_json で解釈)
    CfgSet { json: String },
    /// 保存済み Wi-Fi 設定での接続テスト (結果は `EVT WIFI_TEST ...`)
    WifiTest,
    /// BLE の全ボンド消去 → 次接続で再ペアリング (血圧計の暗号化接続復旧)
    BlePair,
    /// device credential の直接注入 (USB 前提の provisioning — ホストが
    /// auth-worker `/device/pair` 系で取得した credential をシリアルで渡す)
    AuthSet {
        device_id: String,
        device_secret: String,
        tenant_id: String,
    },
    /// 保存済み device credential の破棄 (ローカルのみ。サーバ側 revoke は
    /// operator が auth-worker で行う)
    AuthUnpair,
    /// ペアリング状態の問い合わせ (`AUTH PAIRED ...` / `AUTH UNPAIRED` を応答)
    AuthStatus,
    /// auth-worker ベース URL の上書き (staging テスト用。NVS 保存)
    AuthUrl { url: String },
    /// 保存済み credential で device JWT を取得する自己診断
    AuthToken,
    /// 端末登録の一回券を auth-worker から取得する (ippoan/auth-worker#519、
    /// ippoan/alc-app-s3#204)。運行者 PWA が USB 経由で受け取り、管理者ログイン
    /// 無しで端末登録に使う。secret / JWT はホストへ出さず、券だけを返す
    AuthTicket,
    /// ed25519 鍵対を機体内で生成 (`FORCE` で既存を上書き)。秘密鍵は NVS のみ
    /// に留まり、USB には公開鍵しか出さない (Refs #205)
    AuthKeygen { force: bool },
    /// 生成済み公開鍵の提示 (`AUTH PUBKEY <base64url>` / 無ければエラー)
    AuthPubkey,
    /// サーバの nonce (小文字 hex 32 文字) に署名する。**署名対象は nonce その
    /// もの** — 管理者ログイン (`/auth/device-login`) がこの署名を使うので、
    /// ここに何かを足さないこと (Refs #249)
    AuthSign { nonce: String },
    /// サーバの nonce に**血圧計のボンド状態を束縛して**署名する (Refs #249)。
    /// キオスク端末の認証 (`/device/alarm-token`) 用で、[`HostCommand::AuthSign`]
    /// とは応答 prefix ごと別の口。古いファームは知らないコマンドとしてエラーを
    /// 返し、ホストが `AUTH SIGN` へフォールバックする
    AuthSignBp { nonce: String },
    /// cf-alc-recorder WS URL の上書き (staging テスト用。NVS 保存)
    WsUrl { url: String },
    /// WS 送信の状態問い合わせ (`WS CONNECTED=1 QUEUE=3 SEQ=42` を応答)
    WsStatus,
    /// Windows GW (alc-gw) ハブ URL の保存 (`GW URL ws://<GW-IP>:9000`。NVS)
    GwUrl { url: String },
    /// GW 接続の状態問い合わせ (`GW CONNECTED=1 URL=...` を応答)
    GwStatus,
    /// M-Bus 5V を Core 側から出すか (`BUS5V AUTO|ON|OFF`。NVS 保存、既定 AUTO)。
    /// hub-ui の i2c ループが 1 秒ごとに読むので、**反映は即時** (Refs #254)
    Bus5v { mode: Bus5vMode },
    /// M-Bus 5V 出力の問い合わせ
    /// (`BUS5V MODE=auto USB=1 OUT=1 BATTERY=0 BUS_IN=0` を応答)
    Bus5vStatus,
    /// 点呼に血圧を含めるか (`TENKO BP ON|OFF`。NVS 保存、既定 OFF = 保留)
    TenkoBp { enabled: bool },
    /// 点呼構成の問い合わせ (`TENKO BP=0` を応答)
    TenkoStatus,
    /// Omron 血圧計 (HEM-6231T) を拾うか (`OMRON BP ON|OFF`。NVS 保存、既定 OFF)
    OmronBp { enabled: bool },
    /// Omron 構成の問い合わせ (`OMRON BP=0` を応答)
    OmronStatus,
    /// ヒープ状態の問い合わせ
    /// (`HEAP FREE_INT=<n> MIN_INT=<n> FREE_PSRAM=<n> ...` を応答、Refs #27)
    Heap,
    /// ヒープ詳細ダンプ: タスク別スタック余裕 + ヒープブロック概況
    /// (`HEAPDUMP ...` 複数行を応答)
    HeapDump,
    /// 直近ログの吸い出し: `.noinit` リング (crashlog.rs) の現在内容を
    /// `LOGDUMP ...` 複数行で応答する。クラッシュしていなくても呼べるため、
    /// 「LAN が切れた後に原因を取りに行く」用途に使う
    LogDump,
    /// OTA 更新: firmware (app 単体イメージ) の URL からダウンロードして
    /// もう一方の OTA スロットへ書き込み、再起動する (`EVT OTA_* ...` を出力)
    Ota { url: String },
    /// PDF を URL から取得しプリンター 9100 (raw) へストリーミング印刷
    /// (印刷ブリッジ用。宛先は `PRINTER ADDR` で保存済みのもの。
    /// 進捗・結果は `EVT PRINT_* ...`)
    Print { url: String },
    /// プリンター宛先 `host:port` の保存 (NVS)。検証は printer::valid_addr
    PrinterAddr { addr: String },
    /// プリンター宛先の問い合わせ (`PRINTER <addr>` / `PRINTER UNSET` を応答)
    PrinterStatus,
    /// 点呼キオスク (ブラウザ) からの heartbeat (issue #135)。**返信しない**。
    ///
    /// 警告デバイスの「**沈黙を異常とみなす**」設計の入口。キオスクが 3 秒ごとに
    /// 自分の測定系 (FC-1200 / NFC ブリッジ) の生死を 1 ビットに畳んで送り、
    /// 途切れたら端末が自分の判断で鳴る (判定は [`crate::alarm`])。
    /// 命令駆動 (「鳴れ」を送る形) にしないのは、ブラウザ / PC が落ちた
    /// **一番危ないケースで鳴らない**ため (plan/standing-devices.md §4.1)。
    ///
    /// - `HB OK` … 正常
    /// - `HB NG <reason>` … キオスクが異常を自覚している (reason は表示用ラベル)
    /// - 末尾に任意で `call=1` / `call=0` … 点呼の呼び出し (plan §4.2 の案 B)
    /// - 末尾に任意で `grace=<秒>` (1〜120) … **この 1 回だけ**沈黙の猶予を広げる
    ///   (ブラウザが意図した reload の直前に送る、issue #192)。`call=` と順不同
    Heartbeat {
        ok: bool,
        reason: Option<String>,
        call: bool,
        grace: Option<u16>,
    },
    /// 監視停止 (`HB OFF`、`OK HB OFF` を返す)。沈黙警告を**次の `HB OK` まで
    /// 鳴らない**未武装に戻し、USB/JTAG reset を跨いで残す武装フラグも消す
    /// (検証や設置替えで PWA を繋がないときに鳴り続けるのを止める)
    HeartbeatOff,
    /// 指静脈モジュールで読み取る (ippoan/vein-match#20)。成功は 1 行の
    /// `VEIN CHARA <hex>`、失敗は `ERR VEIN <reason>` ([`crate::vein`])。
    /// `vein` feature を持たないビルドは `ERR VEIN: unsupported`
    VeinCapture,
    /// 案内音声を鳴らす (`OK VEIN SAY <x>`)。いつ何を鳴らすかはホストが決める
    VeinSay(crate::vein::VeinVoice),
}

/// PC (運行者タブ) の点呼の段 (`STAGE NFC|TEMP|ALCOHOL|CARINS|PC`)。
/// CoreS3 の点呼画面を PC の流れに合わせるためだけに使う
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostStage {
    /// 免許証待ち → 待機画面
    Nfc,
    /// 体温
    Temp,
    /// アルコール (FC-1200)
    Alcohol,
    /// 電子車検証をタップするか PC で選ぶ段 (免許証の次、#135)
    Carins,
    /// PC の画面だけで進む段 (顔認証・自己申告・日常点検など)
    Pc,
}

impl HostStage {
    /// `OK STAGE <label>` に載せる機械可読ラベル
    pub fn label(self) -> &'static str {
        match self {
            Self::Nfc => "nfc",
            Self::Temp => "temp",
            Self::Alcohol => "alcohol",
            Self::Carins => "carins",
            Self::Pc => "pc",
        }
    }
}

/// heartbeat の理由ラベルとして許す形か (`[a-z0-9_]+`)。
///
/// **`EVT ALARM cause=ng:<reason>` に素通しで出る**ので、空白・大文字・記号は
/// ここで弾いて行プロトコルを壊させない
pub fn valid_hb_reason(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// 画面向きとして有効な角度か
pub fn valid_rotation(deg: u16) -> bool {
    matches!(deg, 0 | 90 | 180 | 270)
}

/// 1 行を解析する。
///
/// - 空行 → `Ok(None)` (無視)
/// - 解析エラー → `Err(ホストへ返す ERR 応答行)`
pub fn parse_line(line: &str, default_qr_timeout_ms: u64) -> Result<Option<HostCommand>, String> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(None);
    }
    let mut it = line.split_whitespace();
    let cmd = it.next().unwrap_or("").to_ascii_uppercase();
    let command = match cmd.as_str() {
        "PING" => HostCommand::Ping,
        "QR" => match it.next() {
            Some(payload) => {
                let timeout_ms = it
                    .next()
                    .and_then(|s| s.parse::<u64>().ok())
                    .map(|s| s * 1000)
                    .unwrap_or(default_qr_timeout_ms);
                HostCommand::ShowQr {
                    payload: payload.to_string(),
                    timeout_ms,
                }
            }
            None => return Err("ERR QR: payload がありません".into()),
        },
        "MEASURE" => HostCommand::Measure,
        "RESULT" => match it.next().map(|s| s.to_ascii_uppercase()).as_deref() {
            Some(v @ ("OK" | "NG")) => HostCommand::Result {
                ok: v == "OK",
                value: it.next().unwrap_or("").to_string(),
            },
            _ => return Err("ERR RESULT: OK|NG が必要です".into()),
        },
        "ERROR" => HostCommand::ShowError {
            message: line
                .splitn(2, char::is_whitespace)
                .nth(1)
                .unwrap_or("")
                .trim()
                .to_string(),
        },
        "RESET" => HostCommand::Reset,
        "ROTATE" => match it.next().and_then(|s| s.parse::<u16>().ok()) {
            Some(deg) if valid_rotation(deg) => HostCommand::Rotate(deg),
            _ => return Err("ERR ROTATE: 0|90|180|270 が必要です".into()),
        },
        "STATUS" => HostCommand::Status,
        "DEVICE" => HostCommand::Device,
        "HEAP" => match it.next().map(|s| s.to_ascii_uppercase()).as_deref() {
            None => HostCommand::Heap,
            Some("DUMP") => HostCommand::HeapDump,
            _ => return Err("ERR HEAP: 引数は DUMP のみ (無引数 = 概況)".into()),
        },
        // 直近ログの吸い出し (障害の事後解析用。crashlog.rs のリング)
        "LOG" => match it.next().map(|s| s.to_ascii_uppercase()).as_deref() {
            Some("DUMP") => HostCommand::LogDump,
            _ => return Err("ERR LOG: 引数は DUMP のみ".into()),
        },
        // OTA 更新 (URL は大文字小文字を保持)
        "OTA" => match it.next() {
            Some(url) if url.starts_with("https://") || url.starts_with("http://") => {
                HostCommand::Ota {
                    url: url.to_string(),
                }
            }
            _ => return Err("ERR OTA: http(s):// で始まる firmware URL が必要です".into()),
        },
        // 印刷 (URL は大文字小文字を保持。宛先は PRINTER ADDR で事前設定)
        "PRINT" => match it.next() {
            Some(url) if url.starts_with("https://") || url.starts_with("http://") => {
                HostCommand::Print {
                    url: url.to_string(),
                }
            }
            _ => return Err("ERR PRINT: http(s):// で始まる PDF URL が必要です".into()),
        },
        "PRINTER" => match it.next().map(|s| s.to_ascii_uppercase()).as_deref() {
            Some("ADDR") => match it.next() {
                Some(addr) if crate::printer::valid_addr(addr) => HostCommand::PrinterAddr {
                    addr: addr.to_string(),
                },
                _ => return Err("ERR PRINTER: ADDR には host:port が必要です".into()),
            },
            Some("STATUS") => HostCommand::PrinterStatus,
            _ => return Err("ERR PRINTER: ADDR|STATUS が必要です".into()),
        },
        "CFG" => match it.next().map(|s| s.to_ascii_uppercase()).as_deref() {
            Some("GET") => HostCommand::CfgGet,
            Some("SET") => {
                // JSON は空白を含み得るため 3 トークン目以降を丸ごと取る
                let json = line
                    .splitn(3, char::is_whitespace)
                    .nth(2)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if json.is_empty() {
                    return Err("ERR CFG: SET に JSON がありません".into());
                }
                HostCommand::CfgSet { json }
            }
            _ => return Err("ERR CFG: GET|SET が必要です".into()),
        },
        "WIFI" => match it.next().map(|s| s.to_ascii_uppercase()).as_deref() {
            Some("TEST") => HostCommand::WifiTest,
            _ => return Err("ERR WIFI: TEST が必要です".into()),
        },
        // 測定データの WS 送信 (cf-alc-recorder)
        "WS" => match it.next().map(|s| s.to_ascii_uppercase()).as_deref() {
            Some("STATUS") => HostCommand::WsStatus,
            Some("URL") => match it.next() {
                Some(url) if url.starts_with("wss://") || url.starts_with("ws://") => {
                    HostCommand::WsUrl {
                        url: url.to_string(),
                    }
                }
                _ => return Err("ERR WS: URL には ws(s):// で始まる URL が必要です".into()),
            },
            _ => return Err("ERR WS: URL|STATUS が必要です".into()),
        },
        // Windows GW (alc-gw) 連携 (gw_link.rs)
        "GW" => match it.next().map(|s| s.to_ascii_uppercase()).as_deref() {
            Some("STATUS") => HostCommand::GwStatus,
            Some("URL") => match it.next() {
                Some(url) if url.starts_with("ws://") || url.starts_with("wss://") => {
                    HostCommand::GwUrl {
                        url: url.to_string(),
                    }
                }
                _ => return Err("ERR GW: URL には ws(s):// で始まる URL が必要です".into()),
            },
            _ => return Err("ERR GW: URL|STATUS が必要です".into()),
        },
        // M-Bus 5V を Core 側から出すか (power.rs set_ext_5v_out)。
        // PoE ベースのように自前で 5V を供給するベースでは OFF にする (#254)
        "BUS5V" => match it.next().map(|s| s.to_ascii_uppercase()).as_deref() {
            Some("STATUS") => HostCommand::Bus5vStatus,
            Some("AUTO") => HostCommand::Bus5v {
                mode: Bus5vMode::Auto,
            },
            Some("ON") | Some("1") => HostCommand::Bus5v {
                mode: Bus5vMode::On,
            },
            Some("OFF") | Some("0") => HostCommand::Bus5v {
                mode: Bus5vMode::Off,
            },
            _ => return Err("ERR BUS5V: AUTO|ON|OFF|STATUS が必要です".into()),
        },
        // 点呼の構成 (血圧はオプション、tenko.rs)
        "TENKO" => match it.next().map(|s| s.to_ascii_uppercase()).as_deref() {
            Some("STATUS") => HostCommand::TenkoStatus,
            Some("BP") => match it.next().map(|s| s.to_ascii_uppercase()).as_deref() {
                Some("ON") | Some("1") => HostCommand::TenkoBp { enabled: true },
                Some("OFF") | Some("0") => HostCommand::TenkoBp { enabled: false },
                _ => return Err("ERR TENKO: BP には ON|OFF が必要です".into()),
            },
            _ => return Err("ERR TENKO: BP|STATUS が必要です".into()),
        },
        // Omron 血圧計 (HEM-6231T) を拾うか (hub-ble)
        "OMRON" => match it.next().map(|s| s.to_ascii_uppercase()).as_deref() {
            Some("STATUS") => HostCommand::OmronStatus,
            Some("BP") => match it.next().map(|s| s.to_ascii_uppercase()).as_deref() {
                Some("ON") | Some("1") => HostCommand::OmronBp { enabled: true },
                Some("OFF") | Some("0") => HostCommand::OmronBp { enabled: false },
                _ => return Err("ERR OMRON: BP には ON|OFF が必要です".into()),
            },
            _ => return Err("ERR OMRON: BP|STATUS が必要です".into()),
        },
        // PC (運行者タブ) の点呼の段。点呼画面を PC の流れに合わせる (hub-ui)
        "STAGE" => match it.next().map(|s| s.to_ascii_uppercase()).as_deref() {
            Some("NFC") => HostCommand::Stage(HostStage::Nfc),
            Some("TEMP") => HostCommand::Stage(HostStage::Temp),
            Some("ALCOHOL") => HostCommand::Stage(HostStage::Alcohol),
            Some("CARINS") => HostCommand::Stage(HostStage::Carins),
            Some("PC") => HostCommand::Stage(HostStage::Pc),
            _ => return Err("ERR STAGE: NFC|TEMP|ALCOHOL|CARINS|PC が必要です".into()),
        },
        // 点呼キオスクからの heartbeat (警告デバイス、issue #135)。返信はしない
        "HB" => {
            let ok = match it.next().map(|s| s.to_ascii_uppercase()).as_deref() {
                Some("OK") => true,
                Some("NG") => false,
                Some("OFF") => {
                    if let Some(tok) = it.next() {
                        return Err(format!("ERR HB: 余分なトークンです: {tok}"));
                    }
                    return Ok(Some(HostCommand::HeartbeatOff));
                }
                _ => return Err("ERR HB: OK|NG|OFF が必要です".into()),
            };
            let mut reason: Option<String> = None;
            let mut call = false;
            let mut grace: Option<u16> = None;
            for tok in it {
                if let Some(v) = tok.strip_prefix("call=") {
                    match v {
                        "1" => call = true,
                        "0" => call = false,
                        _ => return Err("ERR HB: call= には 0|1 が必要です".into()),
                    }
                } else if let Some(v) = tok.strip_prefix("grace=") {
                    // 意図した reload の猶予 (issue #192)。上限は reload → 再接続に
                    // 現実的な範囲に絞る — 大きすぎると本当の沈黙を見逃す
                    match v.parse::<u16>() {
                        Ok(secs) if (1..=120).contains(&secs) => grace = Some(secs),
                        _ => return Err("ERR HB: grace= には 1〜120 が必要です".into()),
                    }
                } else if reason.is_some() {
                    return Err(format!("ERR HB: 余分なトークンです: {tok}"));
                } else if valid_hb_reason(tok) {
                    reason = Some(tok.to_string());
                } else {
                    return Err("ERR HB: reason は [a-z0-9_]+ のみです".into());
                }
            }
            HostCommand::Heartbeat {
                ok,
                reason,
                call,
                grace,
            }
        }
        // 指静脈 (Vein Station の `vein` feature、ippoan/vein-match#20)
        "VEIN" => match it.next().map(|s| s.to_ascii_uppercase()).as_deref() {
            Some("CAPTURE") => HostCommand::VeinCapture,
            Some("SAY") => match it.next().and_then(crate::vein::VeinVoice::parse) {
                Some(v) => HostCommand::VeinSay(v),
                None => {
                    return Err("ERR VEIN: SAY には PLACE|AGAIN|ENROLLED|FAILED が必要です".into())
                }
            },
            _ => return Err("ERR VEIN: CAPTURE|SAY が必要です".into()),
        },
        // `PAIR` または `BLE PAIR`
        "PAIR" => HostCommand::BlePair,
        "BLE" => match it.next().map(|s| s.to_ascii_uppercase()).as_deref() {
            Some("PAIR") => HostCommand::BlePair,
            _ => return Err("ERR BLE: PAIR が必要です".into()),
        },
        // auth-worker デバイス登録 (BLE の PAIR とは別系統)
        "AUTH" => match it.next().map(|s| s.to_ascii_uppercase()).as_deref() {
            Some("SET") => match (it.next(), it.next(), it.next()) {
                (Some(id), Some(secret), Some(tenant)) => HostCommand::AuthSet {
                    device_id: id.to_string(),
                    device_secret: secret.to_string(),
                    tenant_id: tenant.to_string(),
                },
                _ => {
                    return Err(
                        "ERR AUTH: SET には device_id device_secret tenant_id が必要です".into(),
                    )
                }
            },
            Some("UNPAIR") => HostCommand::AuthUnpair,
            Some("STATUS") => HostCommand::AuthStatus,
            Some("TOKEN") => HostCommand::AuthToken,
            Some("TICKET") => HostCommand::AuthTicket,
            Some("URL") => match it.next() {
                Some(url) if url.starts_with("https://") || url.starts_with("http://") => {
                    HostCommand::AuthUrl {
                        url: url.to_string(),
                    }
                }
                _ => return Err("ERR AUTH: URL には http(s):// で始まる URL が必要です".into()),
            },
            Some("KEYGEN") => match it.next().map(|s| s.to_ascii_uppercase()).as_deref() {
                None => HostCommand::AuthKeygen { force: false },
                Some("FORCE") => HostCommand::AuthKeygen { force: true },
                _ => return Err("ERR AUTH: KEYGEN の引数は FORCE のみです".into()),
            },
            Some("PUBKEY") => HostCommand::AuthPubkey,
            Some("SIGN") => match it.next() {
                Some(nonce) => HostCommand::AuthSign {
                    nonce: nonce.to_string(),
                },
                None => return Err("ERR AUTH: SIGN には nonce が必要です".into()),
            },
            Some("SIGNBP") => match it.next() {
                Some(nonce) => HostCommand::AuthSignBp {
                    nonce: nonce.to_string(),
                },
                None => return Err("ERR AUTH: SIGNBP には nonce が必要です".into()),
            },
            _ => {
                return Err(
                    "ERR AUTH: SET|UNPAIR|STATUS|TOKEN|TICKET|URL|KEYGEN|PUBKEY|SIGN|SIGNBP が必要です"
                        .into(),
                )
            }
        },
        _ => return Err(format!("ERR 不明なコマンド: {cmd}")),
    };
    Ok(Some(command))
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: u64 = 60_000; // 既定タイムアウト

    #[test]
    fn empty_and_whitespace_lines_are_ignored() {
        assert_eq!(parse_line("", T), Ok(None));
        assert_eq!(parse_line("   ", T), Ok(None));
    }

    #[test]
    fn ping_is_case_insensitive() {
        assert_eq!(parse_line("ping", T), Ok(Some(HostCommand::Ping)));
    }

    #[test]
    fn qr_with_timeout() {
        assert_eq!(
            parse_line("QR https://example.com/t/abc 30", T),
            Ok(Some(HostCommand::ShowQr {
                payload: "https://example.com/t/abc".into(),
                timeout_ms: 30_000,
            }))
        );
    }

    #[test]
    fn qr_default_timeout() {
        assert_eq!(
            parse_line("QR token123", T),
            Ok(Some(HostCommand::ShowQr {
                payload: "token123".into(),
                timeout_ms: T,
            }))
        );
    }

    #[test]
    fn qr_without_payload_is_error() {
        assert!(parse_line("QR", T).is_err());
    }

    #[test]
    fn measure() {
        assert_eq!(parse_line("MEASURE", T), Ok(Some(HostCommand::Measure)));
    }

    #[test]
    fn result_ok_with_value() {
        assert_eq!(
            parse_line("RESULT OK 0.000", T),
            Ok(Some(HostCommand::Result {
                ok: true,
                value: "0.000".into(),
            }))
        );
    }

    #[test]
    fn result_ng_without_value() {
        assert_eq!(
            parse_line("RESULT ng", T),
            Ok(Some(HostCommand::Result {
                ok: false,
                value: "".into(),
            }))
        );
    }

    #[test]
    fn result_invalid_verdict_is_error() {
        assert!(parse_line("RESULT MAYBE", T).is_err());
        assert!(parse_line("RESULT", T).is_err());
    }

    #[test]
    fn error_with_and_without_message() {
        assert_eq!(
            parse_line("ERROR 通信に失敗しました", T),
            Ok(Some(HostCommand::ShowError {
                message: "通信に失敗しました".into(),
            }))
        );
        assert_eq!(
            parse_line("ERROR", T),
            Ok(Some(HostCommand::ShowError {
                message: "".into(),
            }))
        );
    }

    #[test]
    fn reset_and_status() {
        assert_eq!(parse_line("RESET", T), Ok(Some(HostCommand::Reset)));
        assert_eq!(parse_line("STATUS", T), Ok(Some(HostCommand::Status)));
    }

    #[test]
    fn device() {
        assert_eq!(parse_line("DEVICE", T), Ok(Some(HostCommand::Device)));
    }

    #[test]
    fn host_kind_labels() {
        assert_eq!(HostKind::CoreS3.label(), "cores3");
        assert_eq!(HostKind::AtomS3Print.label(), "atoms3-print");
        assert_eq!(HostKind::Timecard.label(), "timecard");
        assert_eq!(HostKind::Alarm.label(), "alarm");
        assert_eq!(HostKind::BpStation.label(), "bp-station");
    }

    #[test]
    fn host_kind_claim_ticket() {
        assert!(HostKind::CoreS3.claim_ticket());
        assert!(!HostKind::AtomS3Print.claim_ticket());
        assert!(!HostKind::Timecard.claim_ticket());
        assert!(!HostKind::Alarm.claim_ticket());
        assert!(!HostKind::BpStation.claim_ticket());
    }

    #[test]
    fn host_kind_has_board() {
        assert!(HostKind::CoreS3.has_board());
        assert!(!HostKind::AtomS3Print.has_board());
        assert!(!HostKind::Timecard.has_board());
        assert!(!HostKind::Alarm.has_board());
        assert!(!HostKind::BpStation.has_board());
    }

    #[test]
    fn heap() {
        assert_eq!(parse_line("HEAP", T), Ok(Some(HostCommand::Heap)));
        assert_eq!(parse_line("heap", T), Ok(Some(HostCommand::Heap)));
    }

    #[test]
    fn heap_dump() {
        assert_eq!(parse_line("HEAP DUMP", T), Ok(Some(HostCommand::HeapDump)));
        assert_eq!(parse_line("heap dump", T), Ok(Some(HostCommand::HeapDump)));
        assert!(parse_line("HEAP FULL", T).is_err());
    }

    #[test]
    fn log_dump() {
        assert_eq!(parse_line("LOG DUMP", T), Ok(Some(HostCommand::LogDump)));
        assert_eq!(parse_line("log dump", T), Ok(Some(HostCommand::LogDump)));
        // 無引数・未知の引数はどちらもエラー (HEAP と違い無引数の意味は無い)
        assert!(parse_line("LOG", T).is_err());
        assert!(parse_line("LOG TAIL", T).is_err());
    }

    #[test]
    fn ota_url_preserves_case() {
        assert_eq!(
            parse_line("OTA https://Ippoan.github.io/alc-app-s3/firmware/app.bin", T),
            Ok(Some(HostCommand::Ota {
                url: "https://Ippoan.github.io/alc-app-s3/firmware/app.bin".into(),
            }))
        );
        assert_eq!(
            parse_line("ota http://192.168.11.2:8000/app.bin", T),
            Ok(Some(HostCommand::Ota {
                url: "http://192.168.11.2:8000/app.bin".into(),
            }))
        );
    }

    #[test]
    fn ota_errors() {
        assert!(parse_line("OTA", T).is_err());
        assert!(parse_line("OTA ftp://x/app.bin", T).is_err());
        assert!(parse_line("OTA example.com/app.bin", T).is_err());
    }

    #[test]
    fn print_url_preserves_case() {
        assert_eq!(
            parse_line("PRINT https://Example.com/Tenko.pdf", T),
            Ok(Some(HostCommand::Print {
                url: "https://Example.com/Tenko.pdf".into(),
            }))
        );
        assert_eq!(
            parse_line("print http://192.168.11.2:8000/t.pdf", T),
            Ok(Some(HostCommand::Print {
                url: "http://192.168.11.2:8000/t.pdf".into(),
            }))
        );
    }

    #[test]
    fn print_errors() {
        assert!(parse_line("PRINT", T).is_err());
        assert!(parse_line("PRINT ftp://x/t.pdf", T).is_err());
        assert!(parse_line("PRINT example.com/t.pdf", T).is_err());
    }

    #[test]
    fn printer_addr_and_status() {
        assert_eq!(
            parse_line("PRINTER ADDR 192.168.11.60:9100", T),
            Ok(Some(HostCommand::PrinterAddr {
                addr: "192.168.11.60:9100".into(),
            }))
        );
        assert_eq!(
            parse_line("printer status", T),
            Ok(Some(HostCommand::PrinterStatus))
        );
    }

    #[test]
    fn printer_errors() {
        assert!(parse_line("PRINTER", T).is_err());
        assert!(parse_line("PRINTER ADDR", T).is_err());
        assert!(parse_line("PRINTER ADDR hostonly", T).is_err());
        assert!(parse_line("PRINTER ADDR host:0", T).is_err());
        assert!(parse_line("PRINTER RESET", T).is_err());
    }

    #[test]
    fn rotate_valid_angles() {
        for deg in [0u16, 90, 180, 270] {
            assert_eq!(
                parse_line(&format!("ROTATE {deg}"), T),
                Ok(Some(HostCommand::Rotate(deg)))
            );
        }
    }

    #[test]
    fn rotate_invalid_is_error() {
        assert!(parse_line("ROTATE 45", T).is_err());
        assert!(parse_line("ROTATE abc", T).is_err());
        assert!(parse_line("ROTATE", T).is_err());
    }

    #[test]
    fn valid_rotation_domain() {
        assert!(valid_rotation(0));
        assert!(!valid_rotation(45));
    }

    #[test]
    fn unknown_command_is_error() {
        assert_eq!(
            parse_line("FOO bar", T),
            Err("ERR 不明なコマンド: FOO".to_string())
        );
    }

    #[test]
    fn cfg_get_and_set() {
        assert_eq!(parse_line("CFG GET", T), Ok(Some(HostCommand::CfgGet)));
        assert_eq!(
            parse_line(r#"CFG SET {"rotation": 90, "wifi": null}"#, T),
            Ok(Some(HostCommand::CfgSet {
                json: r#"{"rotation": 90, "wifi": null}"#.into(),
            }))
        );
    }

    #[test]
    fn cfg_errors() {
        assert!(parse_line("CFG", T).is_err());
        assert!(parse_line("CFG PUT", T).is_err());
        assert!(parse_line("CFG SET", T).is_err());
        assert!(parse_line("CFG SET   ", T).is_err());
    }

    #[test]
    fn wifi_test() {
        assert_eq!(parse_line("WIFI TEST", T), Ok(Some(HostCommand::WifiTest)));
        assert!(parse_line("WIFI", T).is_err());
        assert!(parse_line("WIFI CONNECT", T).is_err());
    }

    #[test]
    fn ble_pair() {
        assert_eq!(parse_line("PAIR", T), Ok(Some(HostCommand::BlePair)));
        assert_eq!(parse_line("ble pair", T), Ok(Some(HostCommand::BlePair)));
        assert!(parse_line("BLE", T).is_err());
        assert!(parse_line("BLE SCAN", T).is_err());
    }

    #[test]
    fn auth_subcommands() {
        assert_eq!(
            parse_line("auth unpair", T),
            Ok(Some(HostCommand::AuthUnpair))
        );
        assert_eq!(
            parse_line("AUTH STATUS", T),
            Ok(Some(HostCommand::AuthStatus))
        );
        assert_eq!(
            parse_line("AUTH TOKEN", T),
            Ok(Some(HostCommand::AuthToken))
        );
        assert_eq!(
            parse_line("AUTH TICKET", T),
            Ok(Some(HostCommand::AuthTicket))
        );
        assert_eq!(
            parse_line("auth ticket", T),
            Ok(Some(HostCommand::AuthTicket))
        );
    }

    #[test]
    fn auth_set_takes_three_args_case_preserved() {
        assert_eq!(
            parse_line("AUTH SET dev_AbC s3crET-xyz 11111111-2222-3333-4444-555555555555", T),
            Ok(Some(HostCommand::AuthSet {
                device_id: "dev_AbC".into(),
                device_secret: "s3crET-xyz".into(),
                tenant_id: "11111111-2222-3333-4444-555555555555".into(),
            }))
        );
        assert!(parse_line("AUTH SET", T).is_err());
        assert!(parse_line("AUTH SET id", T).is_err());
        assert!(parse_line("AUTH SET id secret", T).is_err());
        // 旧 QR ペアリングの PAIR は廃止 (USB provisioning に一本化)
        assert!(parse_line("AUTH PAIR", T).is_err());
    }

    #[test]
    fn auth_url_preserves_case() {
        assert_eq!(
            parse_line("AUTH URL https://Auth-Staging.ippoan.org", T),
            Ok(Some(HostCommand::AuthUrl {
                url: "https://Auth-Staging.ippoan.org".into(),
            }))
        );
        assert_eq!(
            parse_line("AUTH URL http://192.168.1.10:8787", T),
            Ok(Some(HostCommand::AuthUrl {
                url: "http://192.168.1.10:8787".into(),
            }))
        );
    }

    #[test]
    fn auth_errors() {
        assert!(parse_line("AUTH", T).is_err());
        assert!(parse_line("AUTH REVOKE", T).is_err());
        assert!(parse_line("AUTH URL", T).is_err());
        assert!(parse_line("AUTH URL ftp://x", T).is_err());
        assert!(parse_line("AUTH URL auth.ippoan.org", T).is_err());
    }

    #[test]
    fn auth_keygen_pubkey_sign() {
        assert_eq!(
            parse_line("AUTH KEYGEN", T),
            Ok(Some(HostCommand::AuthKeygen { force: false }))
        );
        assert_eq!(
            parse_line("auth keygen force", T),
            Ok(Some(HostCommand::AuthKeygen { force: true }))
        );
        assert!(parse_line("AUTH KEYGEN BOGUS", T).is_err());
        assert_eq!(parse_line("AUTH PUBKEY", T), Ok(Some(HostCommand::AuthPubkey)));
        assert_eq!(
            parse_line("AUTH SIGN 0123456789abcdef0123456789abcdef", T),
            Ok(Some(HostCommand::AuthSign {
                nonce: "0123456789abcdef0123456789abcdef".into(),
            }))
        );
        assert!(parse_line("AUTH SIGN", T).is_err());
    }

    /// `AUTH SIGNBP` は `AUTH SIGN` とは別の口 (Refs #249)。**`SIGN` に吸われない**
    #[test]
    fn parses_auth_signbp() {
        assert_eq!(
            parse_line("AUTH SIGNBP 0123456789abcdef0123456789abcdef", T),
            Ok(Some(HostCommand::AuthSignBp {
                nonce: "0123456789abcdef0123456789abcdef".into(),
            }))
        );
        // 小文字でも同じ (サブコマンドは大文字化して比べる)
        assert_eq!(
            parse_line("auth signbp 0123456789abcdef0123456789abcdef", T),
            Ok(Some(HostCommand::AuthSignBp {
                nonce: "0123456789abcdef0123456789abcdef".into(),
            }))
        );
        assert!(parse_line("AUTH SIGNBP", T).is_err());
    }

    #[test]
    fn ws_subcommands() {
        assert_eq!(parse_line("WS STATUS", T), Ok(Some(HostCommand::WsStatus)));
        assert_eq!(
            parse_line("ws url wss://alc-recorder-staging.m-tama-ramu.workers.dev/ws", T),
            Ok(Some(HostCommand::WsUrl {
                url: "wss://alc-recorder-staging.m-tama-ramu.workers.dev/ws".into(),
            }))
        );
        assert_eq!(
            parse_line("WS URL ws://192.168.1.10:8787/ws", T),
            Ok(Some(HostCommand::WsUrl {
                url: "ws://192.168.1.10:8787/ws".into(),
            }))
        );
    }

    #[test]
    fn ws_errors() {
        assert!(parse_line("WS", T).is_err());
        assert!(parse_line("WS SEND", T).is_err());
        assert!(parse_line("WS URL", T).is_err());
        assert!(parse_line("WS URL https://x/ws", T).is_err());
    }

    #[test]
    fn gw_subcommands() {
        assert_eq!(parse_line("GW STATUS", T), Ok(Some(HostCommand::GwStatus)));
        assert_eq!(
            parse_line("gw url ws://192.168.11.5:9000", T),
            Ok(Some(HostCommand::GwUrl {
                url: "ws://192.168.11.5:9000".into(),
            }))
        );
        assert_eq!(
            parse_line("GW URL wss://gw.example:9000", T),
            Ok(Some(HostCommand::GwUrl {
                url: "wss://gw.example:9000".into(),
            }))
        );
    }

    #[test]
    fn bus5v_mode_u8_mapping_is_unchanged() {
        // ★ NVS (`bus5v` キー) には現場が #203 以前に設定した値が残っている。
        // この対応を変えると残っている設定を読み違える (#254)
        assert_eq!(Bus5vMode::Auto.to_u8(), 0);
        assert_eq!(Bus5vMode::On.to_u8(), 1);
        assert_eq!(Bus5vMode::Off.to_u8(), 2);
        assert_eq!(Bus5vMode::from_u8(0), Bus5vMode::Auto);
        assert_eq!(Bus5vMode::from_u8(1), Bus5vMode::On);
        assert_eq!(Bus5vMode::from_u8(2), Bus5vMode::Off);
        // 未知の値・未設定は既定 (Auto = #203 以降の固定動作)
        assert_eq!(Bus5vMode::from_u8(3), Bus5vMode::Auto);
        assert_eq!(Bus5vMode::from_u8(255), Bus5vMode::Auto);
        assert_eq!(Bus5vMode::default(), Bus5vMode::Auto);
        assert_eq!(Bus5vMode::Auto.label(), "auto");
        assert_eq!(Bus5vMode::On.label(), "on");
        assert_eq!(Bus5vMode::Off.label(), "off");
    }

    #[test]
    fn bus5v_subcommands() {
        assert_eq!(
            parse_line("BUS5V STATUS", T),
            Ok(Some(HostCommand::Bus5vStatus))
        );
        assert_eq!(
            parse_line("BUS5V AUTO", T),
            Ok(Some(HostCommand::Bus5v {
                mode: Bus5vMode::Auto
            }))
        );
        assert_eq!(
            parse_line("BUS5V ON", T),
            Ok(Some(HostCommand::Bus5v {
                mode: Bus5vMode::On
            }))
        );
        assert_eq!(
            parse_line("bus5v off", T),
            Ok(Some(HostCommand::Bus5v {
                mode: Bus5vMode::Off
            }))
        );
        // 1/0 も受ける (他のトグル系コマンドと同じ)
        assert_eq!(
            parse_line("BUS5V 1", T),
            Ok(Some(HostCommand::Bus5v {
                mode: Bus5vMode::On
            }))
        );
        assert_eq!(
            parse_line("BUS5V 0", T),
            Ok(Some(HostCommand::Bus5v {
                mode: Bus5vMode::Off
            }))
        );
        assert!(parse_line("BUS5V", T).is_err());
        assert!(parse_line("BUS5V MAYBE", T).is_err());
    }

    #[test]
    fn tenko_subcommands() {
        assert_eq!(parse_line("TENKO STATUS", T), Ok(Some(HostCommand::TenkoStatus)));
        assert_eq!(
            parse_line("tenko bp on", T),
            Ok(Some(HostCommand::TenkoBp { enabled: true }))
        );
        assert_eq!(
            parse_line("TENKO BP 1", T),
            Ok(Some(HostCommand::TenkoBp { enabled: true }))
        );
        assert_eq!(
            parse_line("TENKO BP OFF", T),
            Ok(Some(HostCommand::TenkoBp { enabled: false }))
        );
        assert_eq!(
            parse_line("TENKO BP 0", T),
            Ok(Some(HostCommand::TenkoBp { enabled: false }))
        );
    }

    #[test]
    fn tenko_errors() {
        assert!(parse_line("TENKO", T).is_err());
        assert!(parse_line("TENKO TEMP ON", T).is_err());
        assert!(parse_line("TENKO BP", T).is_err());
        assert!(parse_line("TENKO BP MAYBE", T).is_err());
    }

    #[test]
    fn omron_subcommands() {
        assert_eq!(parse_line("OMRON STATUS", T), Ok(Some(HostCommand::OmronStatus)));
        assert_eq!(
            parse_line("omron bp on", T),
            Ok(Some(HostCommand::OmronBp { enabled: true }))
        );
        assert_eq!(
            parse_line("OMRON BP 1", T),
            Ok(Some(HostCommand::OmronBp { enabled: true }))
        );
        assert_eq!(
            parse_line("OMRON BP OFF", T),
            Ok(Some(HostCommand::OmronBp { enabled: false }))
        );
        assert_eq!(
            parse_line("OMRON BP 0", T),
            Ok(Some(HostCommand::OmronBp { enabled: false }))
        );
    }

    #[test]
    fn omron_errors() {
        assert!(parse_line("OMRON", T).is_err());
        assert!(parse_line("OMRON TEMP ON", T).is_err());
        assert!(parse_line("OMRON BP", T).is_err());
        assert!(parse_line("OMRON BP MAYBE", T).is_err());
    }

    #[test]
    fn stage_subcommands() {
        assert_eq!(
            parse_line("STAGE NFC", T),
            Ok(Some(HostCommand::Stage(HostStage::Nfc)))
        );
        assert_eq!(
            parse_line("STAGE TEMP", T),
            Ok(Some(HostCommand::Stage(HostStage::Temp)))
        );
        assert_eq!(
            parse_line("STAGE ALCOHOL", T),
            Ok(Some(HostCommand::Stage(HostStage::Alcohol)))
        );
        assert_eq!(
            parse_line("STAGE CARINS", T),
            Ok(Some(HostCommand::Stage(HostStage::Carins)))
        );
        assert_eq!(
            parse_line("STAGE PC", T),
            Ok(Some(HostCommand::Stage(HostStage::Pc)))
        );
        // 小文字でも通る (他コマンドと同じ大文字小文字非依存)
        assert_eq!(
            parse_line("stage temp", T),
            Ok(Some(HostCommand::Stage(HostStage::Temp)))
        );
        assert_eq!(
            parse_line("stage carins", T),
            Ok(Some(HostCommand::Stage(HostStage::Carins)))
        );
    }

    #[test]
    fn stage_errors() {
        assert_eq!(
            parse_line("STAGE", T),
            Err("ERR STAGE: NFC|TEMP|ALCOHOL|CARINS|PC が必要です".into())
        );
        // 結果は RESULT で送る (STAGE RESULT は無い)
        assert!(parse_line("STAGE RESULT", T).is_err());
        assert!(parse_line("STAGE 1", T).is_err());
    }

    #[test]
    fn stage_labels_are_the_wire_values() {
        assert_eq!(HostStage::Nfc.label(), "nfc");
        assert_eq!(HostStage::Temp.label(), "temp");
        assert_eq!(HostStage::Alcohol.label(), "alcohol");
        assert_eq!(HostStage::Carins.label(), "carins");
        assert_eq!(HostStage::Pc.label(), "pc");
    }

    #[test]
    fn gw_errors() {
        assert!(parse_line("GW", T).is_err());
        assert!(parse_line("GW CONNECT", T).is_err());
        assert!(parse_line("GW URL", T).is_err());
        assert!(parse_line("GW URL http://x:9000", T).is_err());
    }

    fn hb(ok: bool, reason: Option<&str>, call: bool) -> Result<Option<HostCommand>, String> {
        hb_grace(ok, reason, call, None)
    }

    fn hb_grace(
        ok: bool,
        reason: Option<&str>,
        call: bool,
        grace: Option<u16>,
    ) -> Result<Option<HostCommand>, String> {
        Ok(Some(HostCommand::Heartbeat {
            ok,
            reason: reason.map(|s| s.to_string()),
            call,
            grace,
        }))
    }

    #[test]
    fn heartbeat_ok_and_ng() {
        assert_eq!(parse_line("HB OK", T), hb(true, None, false));
        // 小文字でも通る (他コマンドと同じ大文字小文字非依存)
        assert_eq!(parse_line("hb ok", T), hb(true, None, false));
        assert_eq!(
            parse_line("HB NG serial", T),
            hb(false, Some("serial"), false)
        );
        // 理由ラベルは任意 — 無くても受ける (cause 側で既定ラベルに落ちる)
        assert_eq!(parse_line("HB NG", T), hb(false, None, false));
    }

    #[test]
    fn heartbeat_call_flag() {
        assert_eq!(parse_line("HB OK call=1", T), hb(true, None, true));
        assert_eq!(parse_line("HB OK call=0", T), hb(true, None, false));
        assert_eq!(
            parse_line("HB NG nfc_bridge call=1", T),
            hb(false, Some("nfc_bridge"), true)
        );
        assert_eq!(
            parse_line("HB NG nfc_bridge call=0", T),
            hb(false, Some("nfc_bridge"), false)
        );
    }

    #[test]
    fn heartbeat_off() {
        assert_eq!(parse_line("HB OFF", T), Ok(Some(HostCommand::HeartbeatOff)));
        assert_eq!(parse_line("hb off", T), Ok(Some(HostCommand::HeartbeatOff)));
        assert!(parse_line("HB OFF call=1", T).is_err());
    }

    #[test]
    fn heartbeat_rejects_bad_tokens() {
        // OK|NG|OFF が要る
        assert!(parse_line("HB", T).is_err());
        assert!(parse_line("HB MAYBE", T).is_err());
        // reason は EVT ALARM cause=ng:<reason> に素通しで出るので厳しく検査する
        assert!(parse_line("HB NG Serial", T).is_err());
        assert!(parse_line("HB NG se-rial", T).is_err());
        assert!(parse_line("HB NG ng:x", T).is_err());
        // call= の値は 0|1 のみ
        assert!(parse_line("HB OK call=yes", T).is_err());
        // 理由は 1 つだけ
        assert!(parse_line("HB NG serial extra", T).is_err());
    }

    #[test]
    fn heartbeat_grace() {
        // 意図した reload の直前にブラウザが送る形 (issue #192)
        assert_eq!(
            parse_line("HB OK grace=45", T),
            hb_grace(true, None, false, Some(45))
        );
        // 境界 (1〜120)
        assert_eq!(
            parse_line("HB OK grace=1", T),
            hb_grace(true, None, false, Some(1))
        );
        assert_eq!(
            parse_line("HB OK grace=120", T),
            hb_grace(true, None, false, Some(120))
        );
        // call= と順不同、reason との組合せも可
        assert_eq!(
            parse_line("HB OK grace=30 call=1", T),
            hb_grace(true, None, true, Some(30))
        );
        assert_eq!(
            parse_line("HB NG nfc_bridge call=0 grace=60", T),
            hb_grace(false, Some("nfc_bridge"), false, Some(60))
        );
        assert_eq!(
            parse_line("HB NG grace=10 serial", T),
            hb_grace(false, Some("serial"), false, Some(10))
        );
    }

    #[test]
    fn heartbeat_rejects_bad_grace() {
        // grace= の値は 1〜120 の整数のみ
        assert_eq!(
            parse_line("HB OK grace=0", T),
            Err("ERR HB: grace= には 1〜120 が必要です".into())
        );
        assert!(parse_line("HB OK grace=121", T).is_err());
        assert!(parse_line("HB OK grace=999", T).is_err());
        assert!(parse_line("HB OK grace=abc", T).is_err());
        assert!(parse_line("HB OK grace=", T).is_err());
        assert!(parse_line("HB OK grace=-5", T).is_err());
    }

    #[test]
    fn hb_reason_validator() {
        assert!(valid_hb_reason("serial"));
        assert!(valid_hb_reason("nfc_bridge2"));
        assert!(!valid_hb_reason(""));
        assert!(!valid_hb_reason("Serial"));
        assert!(!valid_hb_reason("se rial"));
    }

    #[test]
    fn vein_commands() {
        use crate::vein::VeinVoice;
        assert_eq!(
            parse_line("VEIN CAPTURE", T),
            Ok(Some(HostCommand::VeinCapture))
        );
        assert_eq!(
            parse_line("vein capture", T),
            Ok(Some(HostCommand::VeinCapture))
        );
        assert_eq!(
            parse_line("VEIN SAY PLACE", T),
            Ok(Some(HostCommand::VeinSay(VeinVoice::Place)))
        );
        assert_eq!(
            parse_line("VEIN SAY enrolled", T),
            Ok(Some(HostCommand::VeinSay(VeinVoice::Enrolled)))
        );
        assert_eq!(
            parse_line("VEIN SAY", T),
            Err("ERR VEIN: SAY には PLACE|AGAIN|ENROLLED|FAILED が必要です".into())
        );
        assert!(parse_line("VEIN SAY HELLO", T).is_err());
        assert_eq!(
            parse_line("VEIN", T),
            Err("ERR VEIN: CAPTURE|SAY が必要です".into())
        );
        assert!(parse_line("VEIN ENROLL", T).is_err());
    }
}
