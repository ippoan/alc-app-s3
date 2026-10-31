//! 指静脈モジュール (Waveshare Finger Vein Scanner Module、XG 系) の UART
//! プロトコル — **純粋部分** (ippoan/vein-match#20)。
//!
//! Vein Station (Atom VoiceS3R + `atoms3-timecard` の `vein` feature) が
//! モジュールから特徴量を取り出し、`VEIN CHARA <hex>` 行でホスト (alc-app の
//! Web Serial) へ渡すための手順。UART の読み書きは [`Port`] の向こう
//! (`alc-hub-drivers::vein`) にあり、ここはパケットの組み立て・検査と
//! 「接続 → 作業用 ID に 1 回登録 → 登録データの分割読み出し → 作業用 ID を消す」の
//! 進め方だけを持つ (ホストで `cargo test`)。
//!
//! # 出典と実機 (2026-10-07、Waveshare Finger Vein Scanner Module (A)、名乗りは `WS-FVS10`)
//!
//! 仕様書 Communication Protocol Ver 2.3 (§1.2 パケット・§1.2.6 分割読み出し・
//! §2.2.1 接続・§2.2.15 削除・§2.2.20 登録・§2.2.26 登録データ読み出し・付録のエラーコード)。
//! 実機で確かめたこと (ippoan/vein-base#105):
//!
//! - **GET_CHARA (0x28) は非対応** (`01 10` = XG_ERR_NO_SUPPORT)。仕様書には UART でも
//!   使えるとあるが、この機種では使えない。そのため特徴量は「作業用 ID ([`SCRATCH_ID`]) へ
//!   1 回だけ登録 → READ_ENROLL (0x22) で大きさ → READ_DATA の種別 0x22 で読む」で取る
//!   (旧経路の `VEIN CAPTURE`。照合はサーバー)
//! - **READ_DATA の応答は「生データ + 2 バイトの和」だけ**で、応答パケットを前に挟まない
//! - **大きさは `bData[1] + bData[2] * 256`** (実機で 0x1FDC = 8156 バイト。先頭は `DE ED DE ED`)
//! - 登録中は「置いて」(0x20) と「離して」(0x21) の途中経過が届く。間は最大 5 秒で、
//!   置かれなければ `01 0B` (XG_ERR_TIME_OUT)
//! - 読めた登録データは 1:N 用のテンプレート (先頭 `DE ED DE ED`、AES の包み) で、
//!   ホスト (alc-vein-worker) が受けるのはその中の **0xBDBD の特徴量 (0x448 バイト) 1 件**。
//!   そのため送る前に [`extract_chara`] で取り出し、`VEIN CHARA` には 0x448 バイト
//!   (2192 文字) だけを載せる (ippoan/vein-base#109)。取り出しは vein-match-search の
//!   `template_to_charas` をそのまま使い、ここでは書き直さない
//!
//! # 照合をモジュールで行う本番の口 (ippoan/alc-app#436 の M-b)
//!
//! 新経路では照合をモジュール自身の 1:N (VERIFY 0) に任せ、モジュールの ID 10〜100 に
//! 乗務員の記録を置く ([`Command`] / [`run`])。`VEIN ENROLL` は登録の直後に置き直して照合し、
//! 全部当たったときだけ記録全体 (READ_ENROLL の全体) を `VEIN RECORD` 行で返す
//! (登録は 1 回ごとに当たり外れがあるため)。サーバーはその記録を保存し、ほかの端末へは
//! `VEIN SYNC WRITE` で書き戻す。ID と乗務員の対応はホスト (画面) が持つ
//!
//! # 指の見張り (ippoan/alc-app#436)
//!
//! 画面が指を置いたら照合を始められるよう、ファームが [`CMD_FINGER_STATE`] (0x10) で指の有無を
//! 見張る ([`FingerWatch`])。**番号と応答の読み方 (`bData[1]` = 1 なら有る、0 なら無い) は推測**
//! — 仕様書の本文が repo に無く、WS-FVS10 で通るかは実機でしか分からない
//! (`VEIN FINGER?` で確かめる)。非対応と決めるのは `01 10` の応答だけで、時間切れ・壊れた応答・
//! 他のエラーコード・想定外のデータは一時的な失敗として見張りを続ける
//!
//! # パケット (24 バイト、§1.2.2)
//!
//! ```text
//! BB AA | addr | cmd | encode | len | data[16] | sum(LE, 先頭 22 バイトの和の下位 16 bit)
//! ```

/// パケット識別子 (`0xAABB` の LE。**BB を先に送る**)
pub const PREFIX: [u8; 2] = [0xBB, 0xAA];
/// パケット長 (識別子 2 + addr/cmd/encode/len 4 + data 16 + 和 2)
pub const PACKET_LEN: usize = 24;
/// パケットのデータ部の長さ
pub const DATA_LEN: usize = 16;

/// 接続 (パスワード。**これ以外のコマンドは接続後でないと使えない**、§2.2.1)
pub const CMD_CONNECTION: u8 = 0x01;
/// 分割読み出し (§1.2.6)
pub const CMD_READ_DATA: u8 = 0x20;
/// 撮像して特徴量を作る (§2.2.28)。**WS-FVS10 は非対応** (冒頭 doc)。送らない
pub const CMD_GET_CHARA: u8 = 0x28;
/// 指定 ID の登録データを消す (§2.2.15)
pub const CMD_CLEAR_ENROLL: u8 = 0x11;
/// 指定 ID に登録する (§2.2.20)。完了まで途中経過が続けて届く
pub const CMD_ENROLL: u8 = 0x16;
/// 指定 ID の登録データの大きさを返す。中身は READ_DATA の種別 0x22 で読む (§2.2.26)
pub const CMD_READ_ENROLL: u8 = 0x22;
/// 書き込み (WRITE_DATA、§2.2.25)。種別は [`CMD_WRITE_ENROLL`] (§2.2.25 の表)。
/// 同期の書き込み (`VEIN SYNC WRITE`) だけが使う
pub const CMD_WRITE_DATA: u8 = 0x21;
/// 指定 ID の登録データを書く前の確認 (WRITE_ENROLL、§2.2.27。ID と大きさを渡す)
pub const CMD_WRITE_ENROLL: u8 = 0x23;
/// 登録ユーザー数と上限 (GET_ENROLL_INFO、§2.2.18。送る `bData` は無し)。
/// 試験用 (`VEIN TEST CAPACITY`) だけが使う
pub const CMD_GET_ENROLL_INFO: u8 = 0x14;

/// モジュールの動作記録 (文字) の大きさを返す (GET_DEBUG)。中身は READ_DATA の種別 `0x26` で読む。
/// 照合のたびに `Match.User:` / `->A:` / `PS_Search OK:` / `USER:` の行が残る ([`parse_match_log`])。
/// **仕様書に節は無いが、`VEIN TEST RAW` で実機に通じることを確かめてある** (ippoan/alc-app#436)。
/// 応答の読み方は READ_ENROLL と同じ形 (`bData[1] + bData[2] * 256` が大きさ) と**推測**している。
/// 照合の成功の直後に 1 回だけ使う ([`read_match_score`])
pub const CMD_GET_DEBUG: u8 = 0x26;
/// 動作記録の大きさの上限 (実機は 4096 バイト)。これを超える大きさは応答の破損とみなす
pub const DEBUG_LOG_MAX: usize = 4096;
/// 動作記録を読む 1 回の待ち (ミリ秒)。点数は付け足しなので、返らなければ短く諦める
pub const DEBUG_READ_MS: u32 = 300;

/// モジュールの設定を読む (GET_SYSTEM_INFO、送る `bData` は無し)。試験用 (`VEIN TEST SYSINFO`) だけが使う。
/// **番号と応答の並びは推測** — 仕様書の本文が repo に無い。XG 系では版・機器番号・照合の厳しさ (安全レベル) などの
/// 設定値が応答のデータ部に並ぶのが通例なので、データ部をそのまま返して実機で読む (ippoan/alc-app#436)
pub const CMD_GET_SYSTEM_INFO: u8 = 0x03;

/// 撮影して生画像の幅と高さを返す (READ_IMAGE、仕様書の表に無い。送る `bData` は 6 バイトの 0)。
/// 画像そのものは READ_DATA の種別 `0x24` で読む。320×240・8bit グレースケールのはず。
/// **DLL の解析 (vein-match `docs/research/waveshare-finger-vein.md` §3) からの推測で、この機種の
/// 実機で通るかは未確認**。試験用 (`VEIN TEST IMAGE`) だけが使う (ippoan/alc-app#436)
pub const CMD_READ_IMAGE: u8 = 0x24;
/// 生画像の大きさの上限 (幅 × 高さ)。320×240 = 76,800 に余裕を持たせる
pub const IMAGE_MAX: usize = 0x20000;

/// 照合する (§2.2.21)。`bData[0..4]` = ID LE。**ID が 1 以上なら 1:1、`0` なら 1:N** で、
/// 1:N の成功時は応答の `bData[1..5]` (LE) が当たった ID。途中経過 (置いて / 離して) は
/// ENROLL と同じく届く。2026-10-21 の実機 (0.1.5) で 1:1・1:N とも通り、不一致は `01 0C`
/// ([`XG_ERR_VERIFY`])。非対応は `01 10`
pub const CMD_VERIFY: u8 = 0x17;

/// 指の状態の問い合わせ (XG 系の「指の状態」)。**番号は推測 (0x10) — 仕様書の本文が repo に無く、
/// WS-FVS10 で通るかは実機でしか分からない**。`VEIN FINGER?` で確かめる ([`finger_state`])
pub const CMD_FINGER_STATE: u8 = 0x10;
/// コマンド非対応 (`XG_ERR_NO_SUPPORT`、`bData[1]`)。GET_CHARA で実機確認済みの応答
pub const XG_ERR_NO_SUPPORT: u8 = 0x10;

/// 特徴量を取り出すために 1 回だけ登録する作業用の ID。読み取りの前後で消す
pub const SCRATCH_ID: u32 = 1;
/// 1 回の登録で撮るテンプレートの数 (`bData[5]`)。読み取り 1 回 = 撮像 1 回
pub const ENROLL_TEMPLATES: u8 = 1;
/// 本番の登録 (`VEIN ENROLL`) で撮るテンプレートの数。1 本だと、登録したときと置き方が少し変わったり
/// 別の端末のモジュールだったりすると照合の最初のふるいで落ちていた。3 本 (メーカーの試験ソフトと同じ)
/// で別の端末の記録が 1:1 で 2/4 通るようになったので、4 本を試す。仕様書は「最大 3 本」と書くが、
/// この機種 (XG-NV7S、1 人 6 本まで) は 4 本・6 本の登録を受け付けた (ippoan/alc-app#436)
pub const ENROLL_TEMPLATES_PRODUCTION: u8 = 4;
/// 撮像に失敗したときにモジュールが撮り直す回数 (`bData[10]`)。1 回の待ちは最大 5 秒なので、
/// ホスト (alc-app `useVeinSerial` の `CAPTURE_TIMEOUT_MS` = 20 秒) に収まる回数にする
pub const ENROLL_RETRIES: u8 = 2;
/// 本番の登録 (テンプレートを複数撮る) で、**1 回置くごとに**撮り直す回数 (`bData[10]`)。
/// 1 枚撮れた「離して」で数え直す ([`send_and_await_final`])。[`ENROLL_RETRIES`] の 2 回で
/// 4 回置く登録の全体を数えていたので、4 回のどこかで撮り直しが 3 回目に入ると登録ごと
/// `RC=11` (静脈が写らない) で終わっていた (ippoan/alc-app#436)。照合・読み取りは 2 回のまま
/// (撮り直しのたびに指を最長 15 秒待つので、増やすと待ちが長くなる)
pub const ENROLL_PLACEMENT_RETRIES: u8 = 5;

/// 試験用コマンド (`VEIN TEST …`) が使える ID の範囲。**1 は読み取りの作業用 ([`SCRATCH_ID`])
/// で毎回消されるので使わない**
pub const TEST_ID_MIN: u32 = 2;
pub const TEST_ID_MAX: u32 = 9;
/// 本番の口 (`VEIN ENROLL` / `VEIN SYNC …`) が使う ID の範囲。モジュールの上限は 100 人
/// (2026-10-21 実機の GET_ENROLL_INFO)。1 は作業用 ([`SCRATCH_ID`])、2〜9 は試験用
/// ([`TEST_ID_MIN`]〜[`TEST_ID_MAX`]。確認タブの試験が本番の登録を上書きしないよう重ねない)
pub const MODULE_ID_MIN: u32 = 10;
pub const MODULE_ID_MAX: u32 = 100;
/// 本番の登録で置く回数の合計 (= 撮影の本数。登録の後の確認の照合はしない — 仕様は「最初の登録は
/// 4 回置く」)。`EVT VEIN PLACE <n> <合計>` の合計
pub const ENROLL_PLACEMENTS: u8 = ENROLL_TEMPLATES_PRODUCTION;
/// `VEIN RECORD` 1 行に載せる記録のバイト数 (16 進で 2048 文字)。最長の行
/// (`VEIN RECORD 100 7168 ` + 2048 文字 = 2069 文字) が、実機で通っている `VEIN CHARA`
/// (2203 文字) より短くなる大きさ
pub const RECORD_LINE_MAX: usize = 1024;
/// `VEIN TEST VERIFY` の応答で流す `DATA` の上限 (バイト)。特徴量のような大きなデータを
/// 流さないために先頭だけで切る
pub const VERIFY_DATA_MAX: usize = 32;

/// 工場出荷時の接続パスワード ("0" × 8、§2.2.1)
pub const DEFAULT_PASSWORD: &[u8; 8] = b"00000000";

/// 成功
pub const XG_ERR_SUCCESS: u8 = 0x00;
/// 失敗 (`bData[1]` が理由のエラーコード)
pub const XG_ERR_FAIL: u8 = 0x01;
/// 指の入力待ちがモジュール側で時間切れ
pub const XG_ERR_TIME_OUT: u8 = 0x0B;
/// 照合の不一致 (`XG_ERR_VERIFY`、VERIFY の `bData[1]`)
pub const XG_ERR_VERIFY: u8 = 0x0C;
/// 登録中、撮れた指が前の撮像と違う。**終わりではない** — モジュールが撮り直す (§2.2.20 の例)
pub const XG_ERR_NO_SAME_FINGER: u8 = 0x09;
/// 登録中、静脈が撮れなかった (置き方が浅い等)。**終わりではない** — モジュールが撮り直す
/// (§2.2.20 の例: `bData[1]` が 0x11 / 0x09 のときは次の応答を待ち続ける。2026-10-07 実機で発生)
pub const XG_ERR_NO_VEIN: u8 = 0x11;
/// 「指を置いて」の途中経過 (登録中に届く)
pub const XG_INPUT_FINGER: u8 = 0x20;
/// 「指を離して」の途中経過 (登録中に届く)
pub const XG_RELEASE_FINGER: u8 = 0x21;

/// READ_DATA の 1 回の上限 (**UART は 512 バイト**。USB は 4096、§1.2.6)
pub const UART_DATA_PACKET_MAX: usize = 512;
/// 特徴量 (登録データ) として受け取る大きさの上限。実機の登録データは 0x1FDC (8156) バイト
/// (2026-10-07) なので 8KB とする — これを超える値は応答の破損とみなす
pub const CHARA_MAX: usize = 0x2000;
/// ホストへ送る特徴量 (0xBDBD の構造体) の大きさ = 0x448 バイト。登録データ
/// ([`CHARA_MAX`] まで) から [`extract_chara`] で取り出す
pub const CHARA_LEN: usize = vein_match::REC_LEN;

/// パケットのデータ部 (`bDataLen` 以降の 16 バイト) を足し合わせる和 (§1.2.3)。
/// 下位 16 bit だけを使う
pub fn checksum(bytes: &[u8]) -> u16 {
    bytes
        .iter()
        .fold(0u16, |acc, &b| acc.wrapping_add(u16::from(b)))
}

/// コマンドパケットを組み立てる (アドレス 0 = 単体接続、encode 0)。
///
/// `data` は 16 バイトまで (超えた分は捨てる)。`bDataLen` は実際に載せた長さ
pub fn command(cmd: u8, data: &[u8]) -> [u8; PACKET_LEN] {
    let len = data.len().min(DATA_LEN);
    let mut p = [0u8; PACKET_LEN];
    p[..2].copy_from_slice(&PREFIX);
    p[3] = cmd;
    p[5] = len as u8;
    p[6..6 + len].copy_from_slice(&data[..len]);
    let sum = checksum(&p[..PACKET_LEN - 2]);
    p[PACKET_LEN - 2..].copy_from_slice(&sum.to_le_bytes());
    p
}

/// `VEIN SYNC WRITE` 1 行で受ける登録データの最大バイト数 (hex で 384 文字。
/// console の 1 行は 512 バイトで切れるので、前置きを含めて収まる大きさ)
pub const WRITE_LINE_MAX: usize = 192;

/// 登録データの先頭 (`DE ED DE ED`)。書き込む前に確かめる
pub const TEMPLATE_MAGIC: [u8; 4] = [0xDE, 0xED, 0xDE, 0xED];

/// READ_DATA / WRITE_DATA の要求 (§1.2.6 / §2.2.25): `cmd` = [`CMD_READ_DATA`] か
/// [`CMD_WRITE_DATA`]、`bData[0]` = 種別、`[1..4]` = オフセット、`[5..8]` = 大きさ (どちらも LE)。
/// WRITE_DATA はこの後ろに本体と和が続く
pub fn read_data_request(cmd: u8, kind: u8, offset: u32, size: u32) -> [u8; PACKET_LEN] {
    let mut d = [0u8; 9];
    d[0] = kind;
    d[1..5].copy_from_slice(&offset.to_le_bytes());
    d[5..9].copy_from_slice(&size.to_le_bytes());
    command(cmd, &d)
}

/// 応答パケット (識別子と和を確かめたもの)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub cmd: u8,
    pub data: [u8; DATA_LEN],
}

/// パケットとして読めなかった理由
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketError {
    /// 識別子 (`BB AA`) が違う
    Prefix,
    /// 和が合わない
    Checksum,
}

/// 24 バイトを応答パケットとして検査する
pub fn parse_packet(raw: &[u8; PACKET_LEN]) -> Result<Packet, PacketError> {
    if raw[..2] != PREFIX {
        return Err(PacketError::Prefix);
    }
    let want = u16::from_le_bytes([raw[PACKET_LEN - 2], raw[PACKET_LEN - 1]]);
    if checksum(&raw[..PACKET_LEN - 2]) != want {
        return Err(PacketError::Checksum);
    }
    let mut data = [0u8; DATA_LEN];
    data.copy_from_slice(&raw[6..6 + DATA_LEN]);
    Ok(Packet { cmd: raw[3], data })
}

/// READ_DATA で届いた 1 塊 (`size` バイト + 2 バイトの和 LE) を検査し、
/// データ部を返す。和は §1.2.6 のサンプルどおりデータ部だけの和
pub fn verify_chunk(buf: &[u8], size: usize) -> Option<&[u8]> {
    if buf.len() < size + 2 {
        return None;
    }
    let want = u16::from_le_bytes([buf[size], buf[size + 1]]);
    (checksum(&buf[..size]) == want).then_some(&buf[..size])
}

/// `total` バイトを UART の上限 ([`UART_DATA_PACKET_MAX`]) ずつに分けた
/// `(offset, size)` の列 (§1.2.6 の `ReadData` と同じ割り方)
pub fn chunks(total: usize) -> impl Iterator<Item = (usize, usize)> {
    (0..total)
        .step_by(UART_DATA_PACKET_MAX)
        .map(move |off| (off, (total - off).min(UART_DATA_PACKET_MAX)))
}

/// 登録中の途中経過。端末はこれに合わせて合図を鳴らす (`alc-hub-drivers::vein`)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prompt {
    /// 「指を置いて」(`XG_INPUT_FINGER`)
    Place,
    /// 「指を離して」(`XG_RELEASE_FINGER`) = 撮れた
    Release,
    /// 撮れなかった / 違う指だった (`01 11` / `01 09`)。モジュールが撮り直すので置き直してもらう。
    /// 中身はそのエラーコード
    Retry(u8),
    /// `VEIN ENROLL` で置き直してもらう。中身は何回目に置くか (撮影からの通し番号): 撮影の 2 枚目以降は
    /// モジュールが次の指を待ち始めたとき、撮影
    /// [`ENROLL_TEMPLATES_PRODUCTION`] 回なら 2 からその数まで
    Replace(u8),
}

/// ENROLL 中に届いた応答の意味 (§2.2.20)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollReply {
    /// 登録できた
    Done,
    /// 途中経過。次の応答を待つ
    Prompt(Prompt),
    /// 失敗。`bData[1]` のエラーコード
    Failed(u8),
    /// 知らない `bData[0]`
    Unknown(u8),
}

/// ENROLL の応答を読む
pub fn enroll_reply(p: &Packet) -> EnrollReply {
    match p.data[0] {
        XG_ERR_SUCCESS => EnrollReply::Done,
        XG_ERR_FAIL if matches!(p.data[1], XG_ERR_NO_VEIN | XG_ERR_NO_SAME_FINGER) => {
            EnrollReply::Prompt(Prompt::Retry(p.data[1]))
        }
        XG_ERR_FAIL => EnrollReply::Failed(p.data[1]),
        XG_INPUT_FINGER => EnrollReply::Prompt(Prompt::Place),
        XG_RELEASE_FINGER => EnrollReply::Prompt(Prompt::Release),
        other => EnrollReply::Unknown(other),
    }
}

/// 読み取りの失敗。ホストへは `ERR VEIN <reason>` で返す
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VeinError {
    /// 接続コマンドに応答が無い (モジュールが居ない・配線違い・電源なし)
    NoModule,
    /// 応答が途中で途絶えた (端末側の待ち時間切れ)
    Timeout,
    /// 指が置かれないままモジュールが待ちを打ち切った (`XG_ERR_TIME_OUT`)
    NoFinger,
    /// 応答が壊れている (識別子・和・大きさ・読み直しの上限)
    ReadFail,
    /// コマンド非対応 (`01 10`)。VERIFY・指の状態で、この機種では通らないと分かった
    Unsupported,
    /// `VEIN SYNC WRITE`: offset が続いていない・組み立て中のものが無い・ID が違う
    WriteSeq,
    /// `VEIN SYNC WRITE`: 合計が [`CHARA_MAX`] を超える
    WriteTooBig,
    /// `VEIN SYNC WRITE`: 先頭が [`TEMPLATE_MAGIC`] でない
    WriteHeader,
    /// モジュールが返したその他のエラーコード
    Rc(u8),
}

impl VeinError {
    /// `ERR VEIN <reason>` の reason
    pub fn reason(self) -> String {
        match self {
            Self::NoModule => "NO_MODULE".into(),
            Self::Timeout => "TIMEOUT".into(),
            Self::NoFinger => "NO_FINGER".into(),
            Self::ReadFail => "READ_FAIL".into(),
            Self::Unsupported => "UNSUPPORTED".into(),
            Self::WriteSeq => "WRITE_SEQ".into(),
            Self::WriteTooBig => "WRITE_TOO_BIG".into(),
            Self::WriteHeader => "WRITE_HEADER".into(),
            Self::Rc(c) => format!("RC={c:02X}"),
        }
    }
}

/// UART の向こう側。実装は `alc-hub-drivers::vein` (テストでは台本どおりに返す偽物)
pub trait Port {
    /// 送る。全部送れなければ false
    fn send(&mut self, bytes: &[u8]) -> bool;
    /// 受信済みで読んでいないバイトを捨てる (前の応答の残りで同期を外さない)
    fn discard_input(&mut self);
    /// 最大 `timeout_ms` 待ち、届いた分を `buf` に読む。読めたバイト数 (0 = 時間切れ)
    fn recv(&mut self, buf: &mut [u8], timeout_ms: u32) -> usize;
    /// `ms` だけ待つ (指が離れるのを見る間隔。UART には触らない)
    fn pause(&mut self, ms: u32);
}

/// 待ち時間 (ミリ秒)。**実機で未確認の値** — 合わなければここだけ直す
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    /// 接続 (と削除・READ_ENROLL) の応答を待つ時間。接続で来なければ [`VeinError::NoModule`]
    pub connect_ms: u32,
    /// ENROLL の応答 (途中経過を含む) 1 つを待つ時間。指を置くまでの
    /// 待ちはモジュール側 (5 秒) が持つので、それより長くとる
    pub finger_ms: u32,
    /// READ_DATA の最初のバイトを待つ時間
    pub data_ms: u32,
    /// パケット・塊の途中でバイトの間が空いてよい時間
    pub idle_ms: u32,
}

impl Timeouts {
    /// 既定値。57600bps で 512 バイト + 2 は約 90ms、24 バイトは約 4ms
    pub const DEFAULT: Self = Self {
        connect_ms: 500,
        finger_ms: 15_000,
        data_ms: 1_000,
        idle_ms: 100,
    };
}

/// 読み直しの回数 (§1.2.6 の `ReadData` と同じく 1 塊につき 3 回まで読み直す)
pub const READ_RETRIES: usize = 3;
/// 登録中に途中経過を受け付ける回数の上限 (応答が途切れず続く故障で
/// スレッドを抱え込まないため)
pub const MAX_PROMPTS: usize = 32;
/// パケットの識別子を探すあいだに読み捨ててよいバイト数
const MAX_GARBAGE: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecvError {
    /// 1 バイトも来なかった
    Silent,
    /// 途中で途絶えた・識別子が見つからない・和が合わない
    Broken,
}

/// `buf` を埋めるまで読む。最初のバイトは `first_ms`、以降は `idle_ms` 待つ
fn recv_exact(
    port: &mut dyn Port,
    buf: &mut [u8],
    first_ms: u32,
    idle_ms: u32,
) -> Result<(), RecvError> {
    let mut got = 0;
    while got < buf.len() {
        let wait = if got == 0 { first_ms } else { idle_ms };
        let n = port.recv(&mut buf[got..], wait);
        if n == 0 {
            return Err(if got == 0 {
                RecvError::Silent
            } else {
                RecvError::Broken
            });
        }
        got += n;
    }
    Ok(())
}

/// 応答パケットを 1 つ読む。識別子 `BB AA` までの前置きのゴミは読み捨てる
fn recv_packet(port: &mut dyn Port, first_ms: u32, idle_ms: u32) -> Result<Packet, RecvError> {
    let mut raw = [0u8; PACKET_LEN];
    let mut byte = [0u8; 1];
    recv_exact(port, &mut byte, first_ms, idle_ms)?;
    let mut prev = byte[0];
    let mut skipped = 0;
    loop {
        recv_exact(port, &mut byte, idle_ms, idle_ms).map_err(|_| RecvError::Broken)?;
        if [prev, byte[0]] == PREFIX {
            break;
        }
        skipped += 1;
        if skipped > MAX_GARBAGE {
            return Err(RecvError::Broken);
        }
        prev = byte[0];
    }
    raw[..2].copy_from_slice(&PREFIX);
    recv_exact(port, &mut raw[2..], idle_ms, idle_ms).map_err(|_| RecvError::Broken)?;
    parse_packet(&raw).map_err(|_| RecvError::Broken)
}

/// 応答が 1 つだけのコマンドを送り、その応答を読む (削除・READ_ENROLL)
fn exchange(port: &mut dyn Port, t: &Timeouts, cmd: u8, data: &[u8]) -> Result<Packet, VeinError> {
    port.discard_input();
    if !port.send(&command(cmd, data)) {
        return Err(VeinError::ReadFail);
    }
    let p = recv_packet(port, t.connect_ms, t.idle_ms).map_err(|e| match e {
        RecvError::Silent => VeinError::Timeout,
        RecvError::Broken => VeinError::ReadFail,
    })?;
    if p.cmd != cmd {
        return Err(VeinError::ReadFail);
    }
    Ok(p)
}

/// 接続する (§2.2.1)。応答が無ければモジュールが居ない ([`VeinError::NoModule`])。
/// 接続以外のコマンドは接続後でないと使えない
pub fn connect(port: &mut dyn Port, t: &Timeouts) -> Result<(), VeinError> {
    port.discard_input();
    if !port.send(&command(CMD_CONNECTION, DEFAULT_PASSWORD)) {
        return Err(VeinError::NoModule);
    }
    let p = recv_packet(port, t.connect_ms, t.idle_ms).map_err(|e| match e {
        RecvError::Silent => VeinError::NoModule,
        RecvError::Broken => VeinError::ReadFail,
    })?;
    if p.cmd != CMD_CONNECTION {
        return Err(VeinError::ReadFail);
    }
    if p.data[0] != XG_ERR_SUCCESS {
        return Err(VeinError::Rc(p.data[1]));
    }
    Ok(())
}

/// ID `id` を空けてから 1 回だけ登録する (§2.2.15 → §2.2.20。接続済みであること)。
/// 読み取り ([`capture`])・登録 ([`enroll`])・試験 ([`test_enroll`]) が共有する。送るバイトは ID だけが違う。
/// 途中経過 (置いて / 離して / 撮り直し) は `on_prompt` へ渡す
fn enroll_id(
    port: &mut dyn Port,
    t: &Timeouts,
    id: u32,
    templates: u8,
    on_prompt: &mut dyn FnMut(Prompt),
) -> Result<(), VeinError> {
    // 前回が途中で終わった残りを消す。未登録なら失敗が返るが、応答さえあればよい
    let id = id.to_le_bytes();
    exchange(port, t, CMD_CLEAR_ENROLL, &id)?;

    let mut req = [0u8; 12];
    req[..4].copy_from_slice(&id);
    req[5] = templates;
    // 1 枚だけ撮る読み取り (作業用) は今までどおり。複数撮る本番の登録は 1 回置くごとに多めに
    let retries = if templates > 1 { ENROLL_PLACEMENT_RETRIES } else { ENROLL_RETRIES };
    req[10] = retries;
    // テンプレートを複数撮るとき、1 枚撮れる (「離して」) たびにモジュールは次の指を待ち直す
    // (`XG_INPUT_FINGER`)。その「置いて」を [`Prompt::Replace`] (n 回目に置く) に替えて、端末が
    // 「もう一度置いてください」を鳴らせるようにする。撮れた直後に決め打ちの間で鳴らすと早すぎる
    // ので、モジュールが次を待ち始めた合図に合わせる (仕様書 §2.2.20 の例は 0.2 秒後に鳴らしている)。
    // 撮り直し (`Retry`) の後の「置いて」は同じ回の置き直しなので数えない
    let mut taken = 0u8;
    let mut released = false;
    let mut relay = |pr: Prompt| match pr {
        Prompt::Release => {
            taken += 1;
            released = true;
            on_prompt(pr);
        }
        Prompt::Place if released => {
            released = false;
            on_prompt(Prompt::Replace(taken + 1));
        }
        _ => on_prompt(pr),
    };
    let p = send_and_await_final(port, t, CMD_ENROLL, &req, retries, &mut relay)?;
    // 途中経過は send_and_await_final が読み切っているので、ここに届くのは成功か失敗だけ。
    // 失敗のコードは `01 xx` なら xx、知らない `bData[0]` ならその値 ([`enroll_reply`] と同じ)
    if p.data[0] == XG_ERR_SUCCESS {
        return Ok(());
    }
    Err(VeinError::Rc(if p.data[0] == XG_ERR_FAIL {
        p.data[1]
    } else {
        p.data[0]
    }))
}

/// `cmd` を送り、途中経過 (置いて / 離して / 撮り直し) を `on_prompt` へ渡しながら
/// 最終の応答を待つ。ENROLL と VERIFY が共有する。
///
/// 返すのは途中経過でない最初の応答 (成功も失敗コードもそのまま。意味づけは呼び出し側)。
/// 指が置かれず時間切れ (`01 0B`) は [`VeinError::NoFinger`]、1 回置く間の撮り直しが
/// `max_retries` 回を超えたら [`VeinError::Rc`] (1 枚撮れた「離して」で数え直す)、
/// 途中経過が [`MAX_PROMPTS`] を超えて続いたら [`VeinError::ReadFail`]
fn send_and_await_final(
    port: &mut dyn Port,
    t: &Timeouts,
    cmd: u8,
    req: &[u8],
    max_retries: u8,
    on_prompt: &mut dyn FnMut(Prompt),
) -> Result<Packet, VeinError> {
    port.discard_input();
    if !port.send(&command(cmd, req)) {
        return Err(VeinError::ReadFail);
    }
    let mut retries = 0u8;
    for _ in 0..=MAX_PROMPTS {
        let p = recv_packet(port, t.finger_ms, t.idle_ms).map_err(|e| match e {
            RecvError::Silent => VeinError::Timeout,
            RecvError::Broken => VeinError::ReadFail,
        })?;
        if p.cmd != cmd {
            return Err(VeinError::ReadFail);
        }
        match enroll_reply(&p) {
            EnrollReply::Prompt(Prompt::Retry(code)) => {
                // 撮り直しはモジュールが max_retries 回まで続ける。それを超えて届いたら終わり
                retries += 1;
                if retries > max_retries {
                    return Err(VeinError::Rc(code));
                }
                on_prompt(Prompt::Retry(code));
            }
            EnrollReply::Prompt(Prompt::Release) => {
                // 1 枚撮れた: 次に置く回の撮り直しは数え直す
                retries = 0;
                on_prompt(Prompt::Release);
            }
            EnrollReply::Prompt(x) => on_prompt(x),
            EnrollReply::Failed(XG_ERR_TIME_OUT) => return Err(VeinError::NoFinger),
            EnrollReply::Done | EnrollReply::Failed(_) | EnrollReply::Unknown(_) => return Ok(p),
        }
    }
    // 途中経過が上限まで続いた = 応答の破損
    Err(VeinError::ReadFail)
}

/// 接続して作業用 ID に 1 回登録し、その登録データを読み出す
/// (§2.2.1 → §2.2.15 → §2.2.20 → §2.2.26 → §1.2.6 → §2.2.15)。
///
/// 戻り値はモジュールが返した大きさそのままの登録データ (ここでは解釈しない。ホストへ
/// 送る特徴量は [`extract_chara`] で取り出す)。
/// 登録中の途中経過は `on_prompt` へ渡す (端末が「離して」の合図を鳴らす)。
/// 案内音声 (置いて / もう一度) はホストが `VEIN SAY` で鳴らす
pub fn capture(
    port: &mut dyn Port,
    t: &Timeouts,
    on_prompt: &mut dyn FnMut(Prompt),
) -> Result<Vec<u8>, VeinError> {
    // 1. 接続。応答が無ければモジュールが居ない
    connect(port, t)?;

    // 2.-3. 作業用 ID を空けて、1 回だけ登録する (途中経過は on_prompt へ)
    enroll_id(port, t, SCRATCH_ID, ENROLL_TEMPLATES, on_prompt)?;
    // 4.-5. 登録データを読む
    let out = read_enroll(port, t, SCRATCH_ID)?;

    // 6. 作業用 ID を消して終わる。失敗しても読めた分は返す (次の読み取りの 2. で消える)
    let _ = exchange(port, t, CMD_CLEAR_ENROLL, &SCRATCH_ID.to_le_bytes());
    Ok(out)
}

/// ID `id` の登録データを読む (接続済みであること。READ_ENROLL → READ_DATA)。
/// 読み取り ([`capture`]) と登録 ([`enroll`]) が共有する
fn read_enroll(port: &mut dyn Port, t: &Timeouts, id: u32) -> Result<Vec<u8>, VeinError> {
    // 登録データの大きさ
    let p = exchange(port, t, CMD_READ_ENROLL, &id.to_le_bytes())?;
    if p.data[0] != XG_ERR_SUCCESS {
        return Err(VeinError::Rc(p.data[1]));
    }
    let size = usize::from(p.data[1]) + usize::from(p.data[2]) * 256;
    if !(1..=CHARA_MAX).contains(&size) {
        return Err(VeinError::ReadFail);
    }

    read_data(port, t, CMD_READ_ENROLL, size)
}

/// 分割読み出し (READ_DATA、種別 `kind`) で `size` バイトを読む。塊ごとに和を確かめ、合わなければ読み直す。
/// 登録データ ([`read_enroll`]) と生画像 ([`test_image`]) が共有する
fn read_data(port: &mut dyn Port, t: &Timeouts, kind: u8, size: usize) -> Result<Vec<u8>, VeinError> {
    let mut out = Vec::with_capacity(size);
    let mut buf = vec![0u8; UART_DATA_PACKET_MAX + 2];
    for (offset, len) in chunks(size) {
        let mut ok = false;
        for _ in 0..READ_RETRIES {
            port.discard_input();
            let req = read_data_request(CMD_READ_DATA, kind, offset as u32, len as u32);
            if !port.send(&req) {
                continue;
            }
            if recv_exact(port, &mut buf[..len + 2], t.data_ms, t.idle_ms).is_err() {
                continue;
            }
            if let Some(data) = verify_chunk(&buf[..len + 2], len) {
                out.extend_from_slice(data);
                ok = true;
                break;
            }
        }
        if !ok {
            return Err(VeinError::ReadFail);
        }
    }
    Ok(out)
}

/// 試験用コマンド (`VEIN TEST …`)。モジュール自身の登録と照合で撮像を確かめる診断で、
/// 運用の画面からは送らない (指静脈の確認タブだけ)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestCommand {
    /// `VEIN TEST ENROLL <id>`: ID に登録してモジュールに残す
    Enroll(u32),
    /// `VEIN TEST VERIFY <id>`: ID で 1:1 照合する。`0` は 1:N (登録済み全員から探す、§2.2.21)
    Verify(u32),
    /// `VEIN TEST CLEAR <id>`: ID の登録を消す
    Clear(u32),
    /// `VEIN TEST CAPACITY`: 登録ユーザー数と登録できる上限を返す
    Capacity,
    /// `VEIN TEST SYSINFO`: モジュールの設定 (GET_SYSTEM_INFO の応答のデータ部) をそのまま返す
    SysInfo,
    /// `VEIN TEST IMAGE`: 撮影して生画像を返す (READ_IMAGE、[`CMD_READ_IMAGE`])
    Image,
    /// `VEIN TEST DEBUG`: モジュールの動作記録 (GET_DEBUG → READ_DATA 0x26) を行ごとに返す ([`test_debug`])。
    /// 点数の取り方 ([`parse_match_log`]) を実機の行で確かめるため
    Debug,
    /// `VEIN TEST RAW <待ち ms> <16 進>`: バイト列をそのまま UART へ送り、返ってきたバイトをそのまま返す
    /// ([`test_raw`])。仕様書にない命令 (指紋互換の EF01 形式、GET_DEBUG 0x26 など) が通じるかを
    /// ホスト側で組み立てて確かめるため (ippoan/alc-app#436)
    Raw { wait_ms: u32, bytes: Vec<u8> },
    /// `VEIN TEST RAWQ <16 進>`: バイト列を送らずに端末の中にためる ([`RawQueue`])。次の `VEIN TEST RAW` が
    /// ためた分の後ろに自分の分をつないで 1 回で流す。指紋互換の DownChar は命令の直後にデータが
    /// 続かないとモジュールが受信を打ち切る (`SynoRevDataEndPkt TIMEOUT`) が、コンソールの 1 行
    /// ([`RAW_SEND_MAX`]) には 1 人分 (700〜850 バイト) が載らないため (ippoan/alc-app#436)
    RawQueue(Vec<u8>),
    /// `VEIN TEST RAWGAP <ms>`: 次からの `VEIN TEST RAW` で、ためた 1 行ずつ (= 1 枠ずつ) の間に置く
    /// 間隔 (0〜[`RAW_GAP_MAX_MS`])。指紋互換の DownChar は、データの枠を詰めて流すと 1 枠ずつ取りこぼし、
    /// 間が空きすぎると打ち切るので、通る間隔を探すため (ippoan/alc-app#436)
    RawGap(u32),
}

/// `VEIN TEST RAW` で一度に送れるバイト数の上限 (SYNC WRITE の 1 行と同じ)
pub const RAW_SEND_MAX: usize = WRITE_LINE_MAX;
/// `VEIN TEST RAW` の最初の 1 バイトを待つ上限 (ミリ秒)。指を待つ命令 (撮影) も試せる長さ
pub const RAW_WAIT_MAX_MS: u32 = 30_000;
/// `VEIN TEST RAWQ` でためられるバイト数の上限
pub const RAW_QUEUE_MAX: usize = 4096;

/// `VEIN TEST RAWGAP` の上限 (ミリ秒)
pub const RAW_GAP_MAX_MS: u32 = 1_000;

/// `VEIN TEST RAWQ` でためたバイト列。1 行 = 1 枠のまま持つ (`VEIN TEST RAWGAP` の間隔は枠の間に置く)。
/// `VEIN TEST RAW` が [`RawQueue::take_with`] で空にする
#[derive(Debug, Default)]
pub struct RawQueue(Vec<Vec<u8>>);

impl RawQueue {
    /// 1 枠として後ろにつなぐ。合わせて [`RAW_QUEUE_MAX`] を超えるなら全部捨てて `Err` (半端なまま流さない)。
    /// 戻り値はためたバイト数
    pub fn push(&mut self, bytes: &[u8]) -> Result<usize, String> {
        if self.len() + bytes.len() > RAW_QUEUE_MAX {
            self.0.clear();
            return Err(format!("ERR VEIN: RAWQ は合わせて {RAW_QUEUE_MAX} バイトまでです (ためた分は捨てた)"));
        }
        self.0.push(bytes.to_vec());
        Ok(self.len())
    }

    fn len(&self) -> usize {
        self.0.iter().map(Vec::len).sum()
    }

    /// ためた枠の後ろに `tail` を最後の枠としてつないで返し、空にする
    pub fn take_with(&mut self, tail: &[u8]) -> Vec<Vec<u8>> {
        let mut all = core::mem::take(&mut self.0);
        all.push(tail.to_vec());
        all
    }
}

/// `VEIN TEST RAWGAP <ms>`
pub fn raw_gap_line(ms: u32) -> String {
    format!("VEIN TEST RAWGAP {ms}")
}

/// `VEIN TEST RAWQ <ためたバイト数>`
pub fn raw_queue_line(len: usize) -> String {
    format!("VEIN TEST RAWQ {len}")
}

/// 本番の口 (ippoan/alc-app#436 の M-b: 照合を端末のモジュールで行う)。結果は [`run`] が返す行
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `VEIN ENROLL <id>`: テンプレート [`ENROLL_TEMPLATES_PRODUCTION`] 本で登録 → 記録全体を返す
    Enroll(u32),
    /// `VEIN IDENTIFY`: 1:N 照合 (VERIFY 0)
    Identify,
    /// `VEIN VERIFY <id>`: 1:1 照合 (VERIFY <id>)。免許証で本人が決まった後に使う
    Verify(u32),
    /// `VEIN SYNC WRITE …`: サーバーの記録をモジュールの ID へ書く
    SyncWrite(WriteCommand),
    /// `VEIN SYNC CLEAR <id>`: ID の登録を消す
    SyncClear(u32),
    /// `VEIN SYNC INFO`: モジュールの登録人数と上限 (モジュールを入れ替えたかの見分けに使う)
    SyncInfo,
}

/// `VEIN SYNC WRITE …`。外から受けた登録データを ID へ書く。
/// **データ (生体情報) を持つので、`Debug` は大きさだけを出す** (ログに hex を残さない)
#[derive(Clone, PartialEq, Eq)]
pub enum WriteCommand {
    /// `VEIN SYNC WRITE <id> <offset> <hex>`: 組み立て中のデータへ続ける (offset 0 は最初から)
    Chunk { id: u32, offset: usize, data: Vec<u8> },
    /// `VEIN SYNC WRITE <id> END`: 組み上がったものを ID へ書く
    End(u32),
}

impl core::fmt::Debug for WriteCommand {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Chunk { id, offset, data } => {
                write!(f, "Chunk(id={id}, offset={offset}, len={})", data.len())
            }
            Self::End(id) => write!(f, "End(id={id})"),
        }
    }
}

/// 外から受けた登録データの組み立て中のもの (ID ごとに 1 つ)。offset の連続と上限を見る
#[derive(Debug, Default)]
pub struct TemplateAssembler {
    id: u32,
    buf: Vec<u8>,
}

impl TemplateAssembler {
    fn new(id: u32) -> Self {
        Self { id, buf: Vec::new() }
    }

    /// 続きを足す。`offset` は今までの長さと一致していること。戻り値は次の offset
    fn push(&mut self, offset: usize, bytes: &[u8]) -> Result<usize, VeinError> {
        if offset != self.buf.len() {
            return Err(VeinError::WriteSeq);
        }
        if self.buf.len() + bytes.len() > CHARA_MAX {
            return Err(VeinError::WriteTooBig);
        }
        self.buf.extend_from_slice(bytes);
        Ok(self.buf.len())
    }
}

/// 数字だけの ID を読み、`min..=max` に入るものだけを返す
fn parse_id_in(s: &str, min: u32, max: u32) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<u32>().ok().filter(|id| (min..=max).contains(id))
}

/// 試験用の ID を読む。[`TEST_ID_MIN`]〜[`TEST_ID_MAX`] の数字だけ (1 は作業用なので不可)
pub fn parse_test_id(s: &str) -> Option<u32> {
    parse_id_in(s, TEST_ID_MIN, TEST_ID_MAX)
}

/// 本番の口の ID を読む。[`MODULE_ID_MIN`]〜[`MODULE_ID_MAX`] の数字だけ (1 は作業用、2〜9 は試験用なので不可)
pub fn parse_module_id(s: &str) -> Option<u32> {
    parse_id_in(s, MODULE_ID_MIN, MODULE_ID_MAX)
}

/// 試験: 接続して ID に登録する。モジュールに残す (READ_ENROLL も削除もしない)
pub fn test_enroll(
    port: &mut dyn Port,
    t: &Timeouts,
    id: u32,
    on_prompt: &mut dyn FnMut(Prompt),
) -> Result<(), VeinError> {
    connect(port, t)?;
    enroll_id(port, t, id, ENROLL_TEMPLATES, on_prompt)
}

/// 試験: 接続して ID で照合する。**ID `0` は 1:N (§2.2.21)**: 登録済み全員から探し、成功なら
/// 応答の `bData[1..5]` (LE) が当たった ID。戻り値は最終応答のデータ部 (`bData[0]` = RC)。
/// 非対応 (`01 10`) は [`VeinError::Unsupported`]
pub fn test_verify(
    port: &mut dyn Port,
    t: &Timeouts,
    id: u32,
    on_prompt: &mut dyn FnMut(Prompt),
) -> Result<[u8; DATA_LEN], VeinError> {
    connect(port, t)?;
    let p = send_and_await_final(port, t, CMD_VERIFY, &id.to_le_bytes(), ENROLL_RETRIES, on_prompt)?;
    if p.data[0] == XG_ERR_FAIL && p.data[1] == XG_ERR_NO_SUPPORT {
        return Err(VeinError::Unsupported);
    }
    Ok(p.data)
}

/// 接続して ID の登録を消す (試験の `VEIN TEST CLEAR` と同期の `VEIN SYNC CLEAR`)。
/// 未登録の ID は失敗コード (`ERR VEIN RC=…`) が返る
pub fn clear(port: &mut dyn Port, t: &Timeouts, id: u32) -> Result<(), VeinError> {
    connect(port, t)?;
    let p = exchange(port, t, CMD_CLEAR_ENROLL, &id.to_le_bytes())?;
    if p.data[0] != XG_ERR_SUCCESS {
        return Err(VeinError::Rc(p.data[1]));
    }
    Ok(())
}

/// `VEIN TEST ENROLLED <id>`
pub fn test_enrolled_line(id: u32) -> String {
    format!("VEIN TEST ENROLLED {id}")
}

/// `VEIN TEST CLEARED <id>`
pub fn test_cleared_line(id: u32) -> String {
    format!("VEIN TEST CLEARED {id}")
}

/// 試験: 接続して登録ユーザー数と上限を読む (GET_ENROLL_INFO、§2.2.18)。
/// 戻り値は (`bData[1..5]` LE = 登録ユーザー数, `bData[9..13]` LE = 上限)
pub fn test_capacity(port: &mut dyn Port, t: &Timeouts) -> Result<(u32, u32), VeinError> {
    connect(port, t)?;
    let p = exchange(port, t, CMD_GET_ENROLL_INFO, &[])?;
    if p.data[0] != XG_ERR_SUCCESS {
        return Err(VeinError::Rc(p.data[1]));
    }
    let word = |i: usize| u32::from_le_bytes([p.data[i], p.data[i + 1], p.data[i + 2], p.data[i + 3]]);
    Ok((word(1), word(9)))
}

/// `VEIN TEST CAPACITY USERS=<登録ユーザー数> MAX=<上限>`
pub fn test_capacity_line(users: u32, max: u32) -> String {
    format!("VEIN TEST CAPACITY USERS={users} MAX={max}")
}

/// 試験: 接続してモジュールの設定を読む (GET_SYSTEM_INFO、[`CMD_GET_SYSTEM_INFO`])。
/// 戻り値は応答のデータ部そのまま (`bData[0]` = RC)。失敗コードも解釈せずに返す (読み方を実機で決めるため)。
/// 非対応 (`01 10`) だけは [`VeinError::Unsupported`]
pub fn test_sysinfo(port: &mut dyn Port, t: &Timeouts) -> Result<[u8; DATA_LEN], VeinError> {
    connect(port, t)?;
    let p = exchange(port, t, CMD_GET_SYSTEM_INFO, &[])?;
    if p.data[0] == XG_ERR_FAIL && p.data[1] == XG_ERR_NO_SUPPORT {
        return Err(VeinError::Unsupported);
    }
    Ok(p.data)
}

/// 試験: 接続して撮影し、生画像を読む ([`CMD_READ_IMAGE`] → READ_DATA 種別 `0x24`)。
/// 戻り値は (幅, 高さ, 画素 (幅 × 高さ バイト))。撮影の途中経過 (置いて / 離して / 撮り直し) は
/// 登録と同じく `on_prompt` へ渡す。非対応 (`01 10`) は [`VeinError::Unsupported`]、他の失敗コードは
/// [`VeinError::Rc`]、幅 × 高さが 0 か [`IMAGE_MAX`] を超えるなら [`VeinError::ReadFail`]
pub fn test_image(
    port: &mut dyn Port,
    t: &Timeouts,
    on_prompt: &mut dyn FnMut(Prompt),
) -> Result<(u16, u16, Vec<u8>), VeinError> {
    connect(port, t)?;
    let p = send_and_await_final(port, t, CMD_READ_IMAGE, &[0u8; 6], ENROLL_RETRIES, on_prompt)?;
    match (p.data[0], p.data[1]) {
        (XG_ERR_SUCCESS, _) => {}
        (XG_ERR_FAIL, XG_ERR_NO_SUPPORT) => return Err(VeinError::Unsupported),
        (XG_ERR_FAIL, code) => return Err(VeinError::Rc(code)),
        (other, _) => return Err(VeinError::Rc(other)),
    }
    let w = u16::from_le_bytes([p.data[1], p.data[2]]);
    let h = u16::from_le_bytes([p.data[3], p.data[4]]);
    let size = usize::from(w) * usize::from(h);
    if !(1..=IMAGE_MAX).contains(&size) {
        return Err(VeinError::ReadFail);
    }
    Ok((w, h, read_data(port, t, CMD_READ_IMAGE, size)?))
}

/// `VEIN TEST DEBUG` の 1 行の長さの上限 (文字)。これを超える行は切る
pub const DEBUG_LINE_MAX: usize = 200;

/// 試験: 接続して動作記録を読み、`VEIN TEST DEBUG <行>` の列と
/// `VEIN TEST DEBUG END <行数>` にする (NUL は落とし、空行は飛ばし、長い行は [`DEBUG_LINE_MAX`] 文字で切る)。
/// 送るのは接続と GET_DEBUG 0x26 / READ_DATA 0x26 だけ ([`read_match_score`] と同じ)
pub fn test_debug(port: &mut dyn Port, t: &Timeouts) -> Result<Vec<String>, VeinError> {
    connect(port, t)?;
    let raw = read_debug_log(port, t)?;
    Ok(debug_lines(&String::from_utf8_lossy(&raw)))
}

/// [`test_debug`] の行づくり
pub fn debug_lines(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text
        .split(['\n', '\r', '\0'])
        .filter(|l| !l.trim().is_empty())
        .map(|l| format!("VEIN TEST DEBUG {}", l.chars().take(DEBUG_LINE_MAX).collect::<String>()))
        .collect();
    lines.push(format!("VEIN TEST DEBUG END {}", lines.len()));
    lines
}

/// `VEIN TEST IMAGE` の成功の行: 画素を [`RECORD_LINE_MAX`] バイトずつ
/// `VEIN TEST IMAGE <offset> <大文字 16 進>` にし、最後に `VEIN TEST IMAGE END <幅> <高さ> <総バイト数>`。
/// 行は読み終えてから続けて出す (途中に `ERR` は挟まらない)
pub fn image_lines(w: u16, h: u16, img: &[u8]) -> Vec<String> {
    use core::fmt::Write;
    let mut lines: Vec<String> = img
        .chunks(RECORD_LINE_MAX)
        .enumerate()
        .map(|(i, part)| {
            let mut s = format!("VEIN TEST IMAGE {} ", i * RECORD_LINE_MAX);
            for b in part {
                let _ = write!(s, "{b:02X}");
            }
            s
        })
        .collect();
    lines.push(format!("VEIN TEST IMAGE END {w} {h} {}", img.len()));
    lines
}

/// 試験: `frames` を順に、枠も和も付けずにそのまま送り (枠の間に `gap_ms` を置く)、返ってきたバイトをそのまま返す。
/// 接続 (CONNECTION) もしない — 送る側が枠ごと組み立てる。最初の 1 バイトを `wait_ms` まで待ち、
/// その後はバイトの間が `t.idle_ms` 空くか [`RECORD_LINE_MAX`] バイトたまるまで読む (0 バイトなら空で返す)
pub fn test_raw(
    port: &mut dyn Port,
    t: &Timeouts,
    wait_ms: u32,
    frames: &[Vec<u8>],
    gap_ms: u32,
) -> Result<Vec<u8>, VeinError> {
    port.discard_input();
    for (i, f) in frames.iter().enumerate() {
        // 枠の間にだけ置く。UART の送信バッファに渡した時点から数える (57600bps で 1 バイト約 0.17ms の
        // 送出時間を含む)
        if i > 0 && gap_ms > 0 {
            port.pause(gap_ms);
        }
        if !port.send(f) {
            return Err(VeinError::ReadFail);
        }
    }
    let mut out = Vec::new();
    let mut buf = [0u8; 256];
    let mut wait = wait_ms;
    while out.len() < RECORD_LINE_MAX {
        let room = buf.len().min(RECORD_LINE_MAX - out.len());
        let n = port.recv(&mut buf[..room], wait);
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
        wait = t.idle_ms;
    }
    Ok(out)
}

/// `VEIN TEST RAW RX=<大文字 16 進>` (何も返らなければ `RX=` のまま)
pub fn test_raw_line(rx: &[u8]) -> String {
    use core::fmt::Write;
    let mut s = String::from("VEIN TEST RAW RX=");
    for b in rx {
        let _ = write!(s, "{b:02X}");
    }
    s
}

/// `VEIN TEST SYSINFO RC=<1 バイト目> DATA=<残り>` (大文字 16 進。生体データは含まない)
pub fn test_sysinfo_line(data: &[u8]) -> String {
    use core::fmt::Write;
    let rc = data.first().copied().unwrap_or(0);
    let mut s = format!("VEIN TEST SYSINFO RC={rc:02X} DATA=");
    for b in data.get(1..).unwrap_or(&[]) {
        let _ = write!(s, "{b:02X}");
    }
    s
}

/// ID `id` の登録データを書く (接続済みであること。WRITE_ENROLL → WRITE_DATA、§2.2.27 / §2.2.25)。
/// `data` は 1 バイト以上 [`CHARA_MAX`] 以下。塊ごとに和を付けて送り、応答が成功でなければ
/// 3 回まで送り直す。**フラッシュに書く**
fn write_enroll(port: &mut dyn Port, t: &Timeouts, id: u32, data: &[u8]) -> Result<(), VeinError> {
    let mut d = [0u8; 8];
    d[..4].copy_from_slice(&id.to_le_bytes());
    d[4..].copy_from_slice(&(data.len() as u32).to_le_bytes());
    let p = exchange(port, t, CMD_WRITE_ENROLL, &d)?;
    if p.data[0] != XG_ERR_SUCCESS {
        return Err(VeinError::Rc(p.data[1]));
    }
    for (offset, len) in chunks(data.len()) {
        let body = &data[offset..offset + len];
        let mut req = read_data_request(CMD_WRITE_DATA, CMD_WRITE_ENROLL, offset as u32, len as u32).to_vec();
        req.extend_from_slice(body);
        req.extend_from_slice(&checksum(body).to_le_bytes());
        let mut ok = false;
        for _ in 0..READ_RETRIES {
            port.discard_input();
            if !port.send(&req) {
                continue;
            }
            let acked = recv_packet(port, t.data_ms, t.idle_ms).is_ok_and(|p| {
                p.cmd == CMD_WRITE_DATA && p.data[0] == XG_ERR_SUCCESS
            });
            if acked {
                ok = true;
                break;
            }
        }
        if !ok {
            return Err(VeinError::ReadFail);
        }
    }
    Ok(())
}

/// ID `id` で 1 回照合する (接続済みであること。`0` は 1:N)。当たれば当たった ID
/// (`bData[1..5]` LE)、不一致 (`01 0C`) は `None`。非対応 (`01 10`) は
/// [`VeinError::Unsupported`]、他の失敗コードは [`VeinError::Rc`]
fn verify(
    port: &mut dyn Port,
    t: &Timeouts,
    id: u32,
    on_prompt: &mut dyn FnMut(Prompt),
) -> Result<Option<u32>, VeinError> {
    let p = send_and_await_final(port, t, CMD_VERIFY, &id.to_le_bytes(), ENROLL_RETRIES, on_prompt)?;
    match (p.data[0], p.data[1]) {
        (XG_ERR_SUCCESS, _) => Ok(Some(u32::from_le_bytes([p.data[1], p.data[2], p.data[3], p.data[4]]))),
        (XG_ERR_FAIL, XG_ERR_VERIFY) => Ok(None),
        (XG_ERR_FAIL, XG_ERR_NO_SUPPORT) => Err(VeinError::Unsupported),
        (XG_ERR_FAIL, code) => Err(VeinError::Rc(code)),
        (other, _) => Err(VeinError::Rc(other)),
    }
}

/// 本番の登録 (`VEIN ENROLL <id>`): 接続 → ID を空けてテンプレート
/// [`ENROLL_TEMPLATES_PRODUCTION`] 本を撮って登録 → 登録データ (記録全体) を読んで返す。
/// 撮影の 2 枚目以降は [`Prompt::Replace`] を `on_prompt` へ渡す。登録の後の確認の照合はしない
/// (同じ指かはモジュールが撮影ごとに見る、`01 09`)。
///
/// 登録の後で読み出しに失敗したら ID を消してから返す — 渡せない登録をモジュールに残さない
pub fn enroll(
    port: &mut dyn Port,
    t: &Timeouts,
    id: u32,
    on_prompt: &mut dyn FnMut(Prompt),
) -> Result<Vec<u8>, VeinError> {
    connect(port, t)?;
    enroll_id(port, t, id, ENROLL_TEMPLATES_PRODUCTION, on_prompt)?;
    let got = read_enroll(port, t, id);
    if got.is_err() {
        let _ = exchange(port, t, CMD_CLEAR_ENROLL, &id.to_le_bytes());
    }
    got
}


/// `VEIN ENROLL` の成功の行: 記録を [`RECORD_LINE_MAX`] バイトずつ
/// `VEIN RECORD <id> <offset> <大文字 16 進>` にし、最後に `VEIN RECORD <id> END <総バイト数>`
pub fn record_lines(id: u32, record: &[u8]) -> Vec<String> {
    use core::fmt::Write;
    let mut lines: Vec<String> = record
        .chunks(RECORD_LINE_MAX)
        .enumerate()
        .map(|(i, part)| {
            let mut s = format!("VEIN RECORD {id} {} ", i * RECORD_LINE_MAX);
            for b in part {
                let _ = write!(s, "{b:02X}");
            }
            s
        })
        .collect();
    lines.push(format!("VEIN RECORD {id} END {}", record.len()));
    lines
}

/// 照合の前に、指が置かれたのを見てから待つ時間 (ミリ秒)。触れた瞬間に撮ると置ききる前になり、
/// 点数の付かない落ち方が増えた (ippoan/alc-app#436 の実測)
pub const IDENTIFY_SETTLE_MS: u32 = 200;
/// 指が置かれるのを待つ間の問い合わせ間隔 (ミリ秒)
pub const SETTLE_POLL_MS: u32 = 50;

/// 照合の前: 指の状態を問い合わせ、「有る」を見たら [`IDENTIFY_SETTLE_MS`] 待つ。
/// 来なければ `t.finger_ms` ほどで諦める。非対応・失敗のときは待たない
/// (どちらも続く VERIFY がモジュールの指待ちで受ける)
fn settle_finger(port: &mut dyn Port, t: &Timeouts) {
    for _ in 0..=t.finger_ms / SETTLE_POLL_MS {
        match finger_state(port, t) {
            FingerReply::Present => return port.pause(IDENTIFY_SETTLE_MS),
            FingerReply::Absent => port.pause(SETTLE_POLL_MS),
            FingerReply::Unsupported | FingerReply::Failed(_) => return,
        }
    }
}

/// 本番の照合 (`VEIN IDENTIFY`): 接続 → 指が置ききるのを待つ → 1:N 照合 (VERIFY 0)。
/// 当たった ID か不一致 (`None`)
pub fn identify(
    port: &mut dyn Port,
    t: &Timeouts,
    on_prompt: &mut dyn FnMut(Prompt),
) -> Result<Option<u32>, VeinError> {
    verify_settled(port, t, 0, on_prompt)
}

/// 本番の 1:1 照合 (`VEIN VERIFY <id>`): [`identify`] と同じ手順で VERIFY <id>
pub fn verify_id(
    port: &mut dyn Port,
    t: &Timeouts,
    id: u32,
    on_prompt: &mut dyn FnMut(Prompt),
) -> Result<Option<u32>, VeinError> {
    verify_settled(port, t, id, on_prompt)
}

fn verify_settled(
    port: &mut dyn Port,
    t: &Timeouts,
    id: u32,
    on_prompt: &mut dyn FnMut(Prompt),
) -> Result<Option<u32>, VeinError> {
    connect(port, t)?;
    settle_finger(port, t);
    verify(port, t, id, on_prompt)
}

/// 照合が通った経路
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchVia {
    /// 静脈 (FV)
    Fv,
    /// 指紋互換 (FP)。FV の点数が 0 でも FP で通ることがある
    Fp,
}

/// 動作記録から読んだ、直近の照合の点数
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchScore {
    /// `<id>->A:<id>, <n>,<点数>` の点数
    pub score: u32,
    /// `USER:<id>, th[s0, s1, s2]` の 3 つ。**どれが何かは未確認** (s0 を合格線として `TH=` に出す)
    pub th: [u32; 3],
    pub via: MatchVia,
}

impl MatchScore {
    /// ` SCORE=<n> TH=<s0> VIA=FV|FP THS=<s0>,<s1>,<s2>` (先頭に空白。応答行の後ろに付ける)
    fn suffix(&self) -> String {
        let via = match self.via {
            MatchVia::Fv => "FV",
            MatchVia::Fp => "FP",
        };
        let [a, b, c] = self.th;
        format!(" SCORE={} TH={a} VIA={via} THS={a},{b},{c}", self.score)
    }
}

fn parse_u32(s: &str) -> Option<u32> {
    s.trim().parse().ok()
}

/// `s` の先頭の数字の並び (符号なし)。数字で始まらなければ `None`
fn leading_u32(s: &str) -> Option<u32> {
    let s = s.trim_start();
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    s[..end].parse().ok()
}

/// 行の `key` の直後の数字 (`FV=12` → 12)。`-1` などの数字でないものは `None`
fn field_after(line: &str, key: &str) -> Option<u32> {
    leading_u32(&line[line.find(key)? + key.len()..])
}

/// `1:1->A:<id>, 6,<点数>` (1:N も `->A:` の形) → 点数。最後のコンマの後ろ
fn score_of(line: &str, id: u32) -> Option<u32> {
    let rest = &line[line.find("->A:")? + 4..];
    let mut fields = rest.split(',');
    (leading_u32(fields.next()?) == Some(id)).then_some(())?;
    parse_u32(fields.next_back()?)
}

/// `USER:<id>, th[s0, s1, s2]` → 3 つ
fn th_of(line: &str, id: u32) -> Option<[u32; 3]> {
    let rest = &line[line.find("USER:")? + 5..];
    (leading_u32(rest) == Some(id)).then_some(())?;
    let inner = &rest[rest.find("th[")? + 3..];
    let mut v = inner[..inner.find(']')?].split(',').map(parse_u32);
    let th = [v.next()??, v.next()??, v.next()??];
    v.next().is_none().then_some(th)
}

/// `PS_Search OK:FV=<id>,FP=<id>` → 通った経路 (FV が当たった ID と同じなら FV、FP だけが同じなら FP)
fn via_of(line: &str, id: u32) -> Option<MatchVia> {
    line.contains("PS_Search OK:").then_some(())?;
    if field_after(line, "FV=") == Some(id) {
        Some(MatchVia::Fv)
    } else if field_after(line, "FP=") == Some(id) {
        Some(MatchVia::Fp)
    } else {
        None
    }
}

/// モジュールの動作記録 (文字) から、当たった ID `id` の**最後の**照合の点数を読む。
///
/// 見る行 (それぞれ記録の中で `id` に当たる最後の行。照合のたびに追記される前提):
/// - `1:1->A:<id>, 6,<点数>`: 点数 = 最後のコンマの後ろ (1:N も `->A:` の形と推測)
/// - `USER:<id>, th[s0, s1, s2]`: 3 つの閾値
/// - `PS_Search OK:FV=<id>,FP=<id>`: FV が `id` なら [`MatchVia::Fv`]、FP だけなら [`MatchVia::Fp`]。
///   この行が無ければ点数 >= s0 で FV、未満なら FP
///
/// 点数か閾値のどちらかが読めなければ `None` (応答に付け足さない)。**実機の記録で形を
/// 確かめていない** — 上の形は ippoan/alc-app#436 の `VEIN TEST RAW` で読んだ行の書き写し
pub fn parse_match_log(text: &str, id: u32) -> Option<MatchScore> {
    let lines = || text.split(['\n', '\r', '\0']);
    let score = lines().rev().find_map(|l| score_of(l, id))?;
    let th = lines().rev().find_map(|l| th_of(l, id))?;
    let via = lines()
        .rev()
        .find_map(|l| via_of(l, id))
        .unwrap_or(if score >= th[0] { MatchVia::Fv } else { MatchVia::Fp });
    Some(MatchScore { score, th, via })
}

/// 動作記録 (GET_DEBUG → READ_DATA 種別 0x26) を読む。接続済みであること
fn read_debug_log(port: &mut dyn Port, t: &Timeouts) -> Result<Vec<u8>, VeinError> {
    let p = exchange(port, t, CMD_GET_DEBUG, &[])?;
    if p.data[0] != XG_ERR_SUCCESS {
        return Err(VeinError::Rc(p.data[1]));
    }
    let size = usize::from(p.data[1]) + usize::from(p.data[2]) * 256;
    if !(1..=DEBUG_LOG_MAX).contains(&size) {
        return Err(VeinError::ReadFail);
    }
    read_data(port, t, CMD_GET_DEBUG, size)
}

/// 照合が通った直後に、当たった ID `id` の点数を動作記録から読む。**付け足しなので何があっても
/// `None`** (時間切れ・壊れた応答・読めない形) — 応答は点数なしの今までの行のまま。待ちは
/// [`DEBUG_READ_MS`] に縮め、失敗したら入力を捨てて次の照合に残さない
fn read_match_score(port: &mut dyn Port, t: &Timeouts, id: u32) -> Option<MatchScore> {
    let quick = Timeouts { connect_ms: DEBUG_READ_MS, data_ms: DEBUG_READ_MS, ..*t };
    let score = read_debug_log(port, &quick)
        .ok()
        .and_then(|raw| parse_match_log(&String::from_utf8_lossy(&raw), id));
    if score.is_none() {
        port.discard_input();
    }
    score
}

/// `VEIN VERIFIED <id>` (点数が読めたら ` SCORE=<n> TH=<n> VIA=FV|FP THS=<s0>,<s1>,<s2>` が続く) /
/// `VEIN NOMATCH`
pub fn verify_line(id: u32, hit: Option<u32>, score: Option<MatchScore>) -> String {
    match hit {
        Some(_) => format!("VEIN VERIFIED {id}{}", score.map(|s| s.suffix()).unwrap_or_default()),
        None => "VEIN NOMATCH".into(),
    }
}

/// `VEIN SYNC INFO USERS=<登録人数> MAX=<上限>`
pub fn sync_info_line(users: u32, max: u32) -> String {
    format!("VEIN SYNC INFO USERS={users} MAX={max}")
}

/// `VEIN IDENTIFIED <id>` (点数が読めたら ` SCORE=<n> TH=<n> VIA=FV|FP THS=<s0>,<s1>,<s2>` が続く) /
/// `VEIN NOMATCH`
pub fn identify_line(hit: Option<u32>, score: Option<MatchScore>) -> String {
    match hit {
        Some(id) => format!("VEIN IDENTIFIED {id}{}", score.map(|s| s.suffix()).unwrap_or_default()),
        None => "VEIN NOMATCH".into(),
    }
}

/// 同期の書き込み: 外から受けた登録データを足す / 書く。`slot` は組み立て中のもの (スレッドが持つ)。
/// offset 0 は組み立てを最初からやり直す。失敗したら組み立て中のものは捨てる。
/// 戻り値は応答の 1 行: 塊なら `VEIN SYNC WRITE <id> <次の offset>`、END なら
/// `VEIN SYNC WRITTEN <id> <バイト数>`。**data を行にもログにも出さない**
pub fn sync_write(
    slot: &mut Option<TemplateAssembler>,
    port: &mut dyn Port,
    t: &Timeouts,
    cmd: WriteCommand,
) -> Result<String, VeinError> {
    match cmd {
        WriteCommand::Chunk { id, offset, data } => {
            let mut asm = match slot.take() {
                Some(a) if offset != 0 && a.id == id => a,
                _ => TemplateAssembler::new(id),
            };
            let next = asm.push(offset, &data)?;
            *slot = Some(asm);
            Ok(format!("VEIN SYNC WRITE {id} {next}"))
        }
        WriteCommand::End(id) => {
            let asm = slot.take().filter(|a| a.id == id).ok_or(VeinError::WriteSeq)?;
            if !asm.buf.starts_with(&TEMPLATE_MAGIC) {
                return Err(VeinError::WriteHeader);
            }
            connect(port, t)?;
            write_enroll(port, t, id, &asm.buf)?;
            Ok(format!("VEIN SYNC WRITTEN {id} {}", asm.buf.len()))
        }
    }
}

/// 本番の口を 1 つ処理し、ホストへ出す行を返す (失敗は `ERR VEIN <reason>` の 1 行)。
/// `slot` は [`sync_write`] の組み立て中のもの。**記録を出すのは `VEIN ENROLL` の成功の
/// `VEIN RECORD` 行だけ** — 失敗の行・ログには出さない
pub fn run(
    cmd: Command,
    slot: &mut Option<TemplateAssembler>,
    port: &mut dyn Port,
    t: &Timeouts,
    on_prompt: &mut dyn FnMut(Prompt),
) -> Vec<String> {
    let lines = match cmd {
        Command::Enroll(id) => enroll(port, t, id, on_prompt).map(|rec| record_lines(id, &rec)),
        Command::Identify => identify(port, t, on_prompt).map(|hit| {
            let score = hit.and_then(|id| read_match_score(port, t, id));
            vec![identify_line(hit, score)]
        }),
        Command::Verify(id) => verify_id(port, t, id, on_prompt).map(|hit| {
            let score = hit.and_then(|id| read_match_score(port, t, id));
            vec![verify_line(id, hit, score)]
        }),
        Command::SyncWrite(w) => sync_write(slot, port, t, w).map(|line| vec![line]),
        Command::SyncClear(id) => clear(port, t, id).map(|()| vec![format!("VEIN SYNC CLEARED {id}")]),
        Command::SyncInfo => test_capacity(port, t).map(|(users, max)| vec![sync_info_line(users, max)]),
    };
    lines.unwrap_or_else(|e| vec![err_line(e)])
}

/// `VEIN TEST VERIFY <id> RC=<1 バイト目> DATA=<残り>` (大文字 16 進)。
/// DATA は先頭 [`VERIFY_DATA_MAX`] バイトまでで切る
pub fn test_verify_line(id: u32, data: &[u8]) -> String {
    use core::fmt::Write;
    let rc = data.first().copied().unwrap_or(0);
    let rest = data.get(1..).unwrap_or(&[]);
    let mut s = format!("VEIN TEST VERIFY {id} RC={rc:02X} DATA=");
    for b in rest.iter().take(VERIFY_DATA_MAX) {
        let _ = write!(s, "{b:02X}");
    }
    s
}

/// 指の状態の問い合わせの結果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FingerReply {
    /// 指が置かれている
    Present,
    /// 置かれていない
    Absent,
    /// コマンド非対応 (`01 10`)。**これだけが「この機種では見張れない」の根拠**
    Unsupported,
    /// 失敗 (時間切れ・壊れた応答・他の失敗コード・データの形が想定外)。一時的とみなす
    Failed(VeinError),
}

/// 指の状態を 1 回問い合わせる ([`CMD_FINGER_STATE`]、接続済みであること)。
///
/// 応答の読み方は**推測**: 成功 (`bData[0]` = 0) のとき `bData[1]` が 1 なら有る、0 なら無い。
/// それ以外の値は想定外として [`FingerReply::Failed`] (`ReadFail`)。実機で確かめるまで
/// 見張りは「想定外」を一時的な失敗として扱い、止まらない
pub fn finger_state(port: &mut dyn Port, t: &Timeouts) -> FingerReply {
    let p = match exchange(port, t, CMD_FINGER_STATE, &[]) {
        Ok(p) => p,
        Err(e) => return FingerReply::Failed(e),
    };
    match (p.data[0], p.data[1]) {
        (XG_ERR_SUCCESS, 0) => FingerReply::Absent,
        (XG_ERR_SUCCESS, 1) => FingerReply::Present,
        (XG_ERR_SUCCESS, _) => FingerReply::Failed(VeinError::ReadFail),
        (XG_ERR_FAIL, XG_ERR_NO_SUPPORT) => FingerReply::Unsupported,
        (XG_ERR_FAIL, code) => FingerReply::Failed(VeinError::Rc(code)),
        _ => FingerReply::Failed(VeinError::ReadFail),
    }
}

/// 見張りの問い合わせ間隔 (ミリ秒)。連続で失敗している間は延ばす
pub const WATCH_INTERVAL_MS: u32 = 200;
/// 連続で失敗しているときの間隔
pub const WATCH_BACKOFF_MS: u32 = 5_000;
/// この回数だけ続けて失敗したら間隔を延ばす
pub const WATCH_BACKOFF_AFTER: u32 = 3;

/// 見張り 1 回の結果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchPoll {
    /// 何も出さない
    Idle,
    /// 指が「無い → 有る」に変わった (`EVT VEIN FINGER`)
    Finger,
    /// 非対応 (`01 10`)。見張りをやめる (`EVT VEIN FINGER UNSUPPORTED`)
    Unsupported,
}

/// 指の見張りの状態。vein スレッドが依頼の無い回に [`FingerWatch::poll`] を呼ぶ
#[derive(Debug, Clone, Default)]
pub struct FingerWatch {
    connected: bool,
    /// 「無い」を見た後か。false の間は「有る」を見ても出さない
    /// (起動時から置かれている指・読み取り直後の同じ指で 2 回目が走らないように)
    armed: bool,
    fails: u32,
}

impl FingerWatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// 次の問い合わせまでの間隔
    pub fn interval_ms(&self) -> u32 {
        if self.fails >= WATCH_BACKOFF_AFTER {
            WATCH_BACKOFF_MS
        } else {
            WATCH_INTERVAL_MS
        }
    }

    /// 連続で失敗している (間隔が延びている) か。ログを状態の変わり目だけにするため
    pub fn is_failing(&self) -> bool {
        self.fails >= WATCH_BACKOFF_AFTER
    }

    /// 読み取り (VEIN CAPTURE) の後。接続し直し、いったん「無い」を見るまで出さない
    pub fn after_capture(&mut self) {
        self.connected = false;
        self.armed = false;
    }

    /// 1 回問い合わせる。接続していなければ先に接続する
    pub fn poll(&mut self, port: &mut dyn Port, t: &Timeouts) -> WatchPoll {
        if !self.connected {
            if connect(port, t).is_err() {
                self.fails = self.fails.saturating_add(1);
                return WatchPoll::Idle;
            }
            self.connected = true;
        }
        match finger_state(port, t) {
            FingerReply::Absent => {
                self.fails = 0;
                self.armed = true;
                WatchPoll::Idle
            }
            FingerReply::Present => {
                self.fails = 0;
                if self.armed {
                    self.armed = false;
                    WatchPoll::Finger
                } else {
                    WatchPoll::Idle
                }
            }
            FingerReply::Unsupported => WatchPoll::Unsupported,
            FingerReply::Failed(_) => {
                // 一時的な失敗。次は接続し直してから
                self.fails = self.fails.saturating_add(1);
                self.connected = false;
                WatchPoll::Idle
            }
        }
    }
}

/// `VEIN FINGER?` の応答行 (改行なし): `VEIN FINGER 0` / `VEIN FINGER 1` /
/// `ERR VEIN UNSUPPORTED` / `ERR VEIN <reason>`
pub fn finger_line(r: FingerReply) -> String {
    match r {
        FingerReply::Present => "VEIN FINGER 1".into(),
        FingerReply::Absent => "VEIN FINGER 0".into(),
        FingerReply::Unsupported => "ERR VEIN UNSUPPORTED".into(),
        FingerReply::Failed(e) => err_line(e),
    }
}

/// 登録データ ([`capture`] の結果。1:N 用のテンプレート、先頭 `DE ED DE ED`) から、
/// ホストへ送る 0xBDBD の特徴量 ([`CHARA_LEN`] バイト) を取り出す。
///
/// 取り出しは vein-match-search の `template_to_charas` (外側ヘッダ → AES の包み → 記録の
/// 各件の先頭 0x448 バイト)。読み取り 1 回 = 撮像 1 回 ([`ENROLL_TEMPLATES`]) なので
/// **件数はちょうど 1**。0 件・2 件以上・形式違い・版違い・未対応の層 (LZW) はどれも
/// 応答の破損として [`VeinError::ReadFail`] にする (ホストの `ERR VEIN` の解釈は変えない)
pub fn extract_chara(enroll: &[u8]) -> Result<[u8; CHARA_LEN], VeinError> {
    match vein_match_search::template_to_charas(enroll).as_deref() {
        Ok([chara]) => Ok(*chara),
        _ => Err(VeinError::ReadFail),
    }
}

/// 成功の行 `VEIN CHARA <大文字 16 進>` (改行なし)
pub fn chara_line(chara: &[u8]) -> String {
    use core::fmt::Write;
    let mut s = String::with_capacity(11 + chara.len() * 2);
    s.push_str("VEIN CHARA ");
    for b in chara {
        let _ = write!(s, "{b:02X}");
    }
    s
}

/// 失敗の行 `ERR VEIN <reason>` (改行なし)
pub fn err_line(e: VeinError) -> String {
    format!("ERR VEIN {}", e.reason())
}

/// `vein` feature を持たないビルドの応答
pub const UNSUPPORTED_LINE: &str = "ERR VEIN: unsupported";

/// `VEIN SAY <x>` の案内音声
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VeinVoice {
    /// 「指を置いてください」
    Place,
    /// 「もう一度置いてください」
    Again,
    /// 「登録完了しました」(既存の音声)
    Enrolled,
    /// 「読み取れませんでした」
    Failed,
}

impl VeinVoice {
    /// 行の語 (大文字小文字は問わない)
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "PLACE" => Some(Self::Place),
            "AGAIN" => Some(Self::Again),
            "ENROLLED" => Some(Self::Enrolled),
            "FAILED" => Some(Self::Failed),
            _ => None,
        }
    }

    /// `OK VEIN SAY <label>` の label
    pub fn label(self) -> &'static str {
        match self {
            Self::Place => "PLACE",
            Self::Again => "AGAIN",
            Self::Enrolled => "ENROLLED",
            Self::Failed => "FAILED",
        }
    }
}

/// 案内音声を受け付けたときの行 `OK VEIN SAY <label>`
pub fn say_ok_line(v: VeinVoice) -> String {
    format!("OK VEIN SAY {}", v.label())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// 仕様書 §1.2.2 の例 (接続): 送信
    const SPEC_CONNECT_SEND: [u8; 24] = [
        0xBB, 0xAA, 0x00, 0x01, 0x00, 0x08, 0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xEE, 0x02,
    ];
    /// 同 受信 (データ部の末尾が 0x02、和 0x039D)
    const SPEC_CONNECT_RECV: [u8; 24] = [
        0xBB, 0xAA, 0x00, 0x01, 0x00, 0x10, 0x00, 0x37, 0x2D, 0x31, 0xB0, 0xE0, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x9D, 0x03,
    ];

    /// 台本どおりに応答する偽の UART。`script` は「送信 1 回ごとに返す応答」の列
    /// (None = 何も返さない)。`noise` は最初の recv の前に混ぜるバイト
    struct FakePort {
        sent: Vec<Vec<u8>>,
        script: VecDeque<Option<Vec<u8>>>,
        rx: VecDeque<u8>,
        /// recv 1 回で渡す上限 (分割到着を再現する)
        max_per_recv: usize,
        send_ok: bool,
        discards: usize,
        pauses: Vec<u32>,
    }

    impl FakePort {
        fn new(script: Vec<Option<Vec<u8>>>) -> Self {
            Self {
                sent: Vec::new(),
                script: script.into(),
                rx: VecDeque::new(),
                max_per_recv: 7,
                send_ok: true,
                discards: 0,
                pauses: Vec::new(),
            }
        }
    }

    impl Port for FakePort {
        fn send(&mut self, bytes: &[u8]) -> bool {
            self.sent.push(bytes.to_vec());
            if !self.send_ok {
                return false;
            }
            if let Some(Some(reply)) = self.script.pop_front() {
                self.rx.extend(reply);
            }
            true
        }
        fn discard_input(&mut self) {
            self.discards += 1;
            self.rx.clear();
        }
        fn recv(&mut self, buf: &mut [u8], _timeout_ms: u32) -> usize {
            let n = buf.len().min(self.rx.len()).min(self.max_per_recv);
            for b in buf.iter_mut().take(n) {
                *b = self.rx.pop_front().unwrap();
            }
            n
        }
        fn pause(&mut self, ms: u32) {
            self.pauses.push(ms);
        }
    }

    fn reply(cmd: u8, data: &[u8]) -> Vec<u8> {
        let mut p = command(cmd, data);
        // 応答の bDataLen はモジュールが決める。和は組み直す
        p[5] = 0x10;
        let sum = checksum(&p[..22]);
        p[22..].copy_from_slice(&sum.to_le_bytes());
        p.to_vec()
    }

    fn chunk(data: &[u8]) -> Vec<u8> {
        let mut v = data.to_vec();
        v.extend_from_slice(&checksum(data).to_le_bytes());
        v
    }

    fn connected() -> Option<Vec<u8>> {
        Some(SPEC_CONNECT_RECV.to_vec())
    }

    /// 削除の応答 (未登録の ID なら失敗が返るが、capture は気にしない)
    fn cleared() -> Option<Vec<u8>> {
        Some(reply(CMD_CLEAR_ENROLL, &[XG_ERR_FAIL, 0x07]))
    }

    /// 登録の応答: 置いて → 離して → 完了 (ENROLL 1 回の送信に続けて届く)
    fn enrolled() -> Option<Vec<u8>> {
        let mut v = reply(CMD_ENROLL, &[XG_INPUT_FINGER]);
        v.extend(reply(CMD_ENROLL, &[XG_RELEASE_FINGER]));
        v.extend(reply(CMD_ENROLL, &[XG_ERR_SUCCESS, 1]));
        Some(v)
    }

    /// 本番の登録 ([`ENROLL_TEMPLATES_PRODUCTION`] 本) の応答。2 枚目で 1 回撮り直しが入る
    fn enrolled_production() -> Option<Vec<u8>> {
        let mut v = Vec::new();
        for k in 0..ENROLL_TEMPLATES_PRODUCTION {
            v.extend(reply(CMD_ENROLL, &[XG_INPUT_FINGER, k]));
            if k == 1 {
                v.extend(reply(CMD_ENROLL, &[XG_ERR_FAIL, XG_ERR_NO_VEIN]));
                v.extend(reply(CMD_ENROLL, &[XG_INPUT_FINGER, k]));
            }
            v.extend(reply(CMD_ENROLL, &[XG_RELEASE_FINGER, k]));
        }
        v.extend(reply(CMD_ENROLL, &[XG_ERR_SUCCESS, ENROLL_TEMPLATES_PRODUCTION]));
        Some(v)
    }

    fn size(n: usize) -> Option<Vec<u8>> {
        Some(reply(CMD_READ_ENROLL, &[0x00, n as u8, (n >> 8) as u8]))
    }

    /// 接続 → 削除 → 登録 → 大きさ `n` までの台本
    fn head(n: usize) -> Vec<Option<Vec<u8>>> {
        vec![connected(), cleared(), enrolled(), size(n)]
    }

    fn run(port: &mut dyn Port) -> Result<Vec<u8>, VeinError> {
        capture(port, &Timeouts::DEFAULT, &mut |_| {})
    }

    /// `n` 回目 (1 始まり) の送信だけ失敗する UART
    struct FailAt(FakePort, usize);
    impl Port for FailAt {
        fn send(&mut self, b: &[u8]) -> bool {
            self.0.send(b) && self.0.sent.len() != self.1
        }
        fn discard_input(&mut self) {
            self.0.discard_input()
        }
        fn recv(&mut self, buf: &mut [u8], t: u32) -> usize {
            self.0.recv(buf, t)
        }
        fn pause(&mut self, ms: u32) {
            self.0.pause(ms)
        }
    }

    /// 実機の登録データの形 (先頭 `DE ED DE ED`) を真似た見本。中身は解釈しないので形だけ
    fn sample_chara(n: usize) -> Vec<u8> {
        let mut v: Vec<u8> = (0..n).map(|i| (i * 7 + 3) as u8).collect();
        v[..4].copy_from_slice(&[0xDE, 0xED, 0xDE, 0xED]);
        v
    }

    fn finger(present: Option<u8>) -> Option<Vec<u8>> {
        present.map(|v| reply(CMD_FINGER_STATE, &[XG_ERR_SUCCESS, v]))
    }

    fn fp(script: Vec<Option<Vec<u8>>>) -> FingerReply {
        finger_state(&mut FakePort::new(script), &Timeouts::DEFAULT)
    }

    #[test]
    fn finger_state_kinds() {
        assert_eq!(fp(vec![finger(Some(1))]), FingerReply::Present);
        assert_eq!(fp(vec![finger(Some(0))]), FingerReply::Absent);
        // コマンド非対応 (01 10) だけが Unsupported
        assert_eq!(
            fp(vec![Some(reply(CMD_FINGER_STATE, &[XG_ERR_FAIL, XG_ERR_NO_SUPPORT]))]),
            FingerReply::Unsupported
        );
        // 他の失敗コード・時間切れ・壊れた応答・想定外のデータ・別コマンドの応答は一時的な失敗
        assert_eq!(
            fp(vec![Some(reply(CMD_FINGER_STATE, &[XG_ERR_FAIL, 0x07]))]),
            FingerReply::Failed(VeinError::Rc(0x07))
        );
        assert_eq!(fp(vec![None]), FingerReply::Failed(VeinError::Timeout));
        assert_eq!(
            fp(vec![Some(SPEC_CONNECT_RECV[..5].to_vec())]),
            FingerReply::Failed(VeinError::ReadFail)
        );
        assert_eq!(fp(vec![finger(Some(7))]), FingerReply::Failed(VeinError::ReadFail));
        assert_eq!(
            fp(vec![Some(reply(CMD_FINGER_STATE, &[0x42]))]),
            FingerReply::Failed(VeinError::ReadFail)
        );
        assert_eq!(
            fp(vec![Some(reply(CMD_CONNECTION, &[0, 1]))]),
            FingerReply::Failed(VeinError::ReadFail)
        );
        let mut port = FakePort::new(vec![]);
        port.send_ok = false;
        assert_eq!(
            finger_state(&mut port, &Timeouts::DEFAULT),
            FingerReply::Failed(VeinError::ReadFail)
        );
    }

    #[test]
    fn finger_state_sends_command_without_data() {
        let mut port = FakePort::new(vec![finger(Some(0))]);
        finger_state(&mut port, &Timeouts::DEFAULT);
        assert_eq!(port.sent, vec![command(CMD_FINGER_STATE, &[]).to_vec()]);
    }

    #[test]
    fn finger_lines() {
        assert_eq!(finger_line(FingerReply::Present), "VEIN FINGER 1");
        assert_eq!(finger_line(FingerReply::Absent), "VEIN FINGER 0");
        assert_eq!(finger_line(FingerReply::Unsupported), "ERR VEIN UNSUPPORTED");
        assert_eq!(
            finger_line(FingerReply::Failed(VeinError::Rc(0x07))),
            "ERR VEIN RC=07"
        );
    }

    #[test]
    fn connect_sends_spec_packet() {
        let mut port = FakePort::new(vec![connected()]);
        assert_eq!(connect(&mut port, &Timeouts::DEFAULT), Ok(()));
        assert_eq!(port.sent[0], SPEC_CONNECT_SEND.to_vec());
    }

    /// 見張りを台本で進める: 接続 1 回 + 問い合わせ n 回
    fn watch_run(script: Vec<Option<Vec<u8>>>, polls: usize) -> (Vec<WatchPoll>, FakePort) {
        let mut port = FakePort::new(script);
        let mut w = FingerWatch::new();
        let out = (0..polls).map(|_| w.poll(&mut port, &Timeouts::DEFAULT)).collect();
        (out, port)
    }

    #[test]
    fn watch_emits_only_on_absent_to_present() {
        // 起動時から指がある → 出さない / 離れた → 置いた で 1 回 / 置いたままなら出さない
        let (out, port) = watch_run(
            vec![
                connected(),
                finger(Some(1)),
                finger(Some(0)),
                finger(Some(1)),
                finger(Some(1)),
                finger(Some(0)),
                finger(Some(1)),
            ],
            6,
        );
        use WatchPoll::*;
        assert_eq!(out, vec![Idle, Idle, Finger, Idle, Idle, Finger]);
        // 接続は最初の 1 回だけ (問い合わせのたびには送らない)
        assert_eq!(port.sent.iter().filter(|p| p[3] == CMD_CONNECTION).count(), 1);
    }

    #[test]
    fn watch_after_capture_needs_absent_and_reconnects() {
        let mut port = FakePort::new(vec![
            connected(),
            finger(Some(0)),
            finger(Some(1)),
            connected(),
            finger(Some(1)),
            finger(Some(0)),
            finger(Some(1)),
        ]);
        let t = Timeouts::DEFAULT;
        let mut w = FingerWatch::new();
        assert_eq!(w.poll(&mut port, &t), WatchPoll::Idle);
        assert_eq!(w.poll(&mut port, &t), WatchPoll::Finger);
        w.after_capture();
        // 撮った直後の同じ指では出さない (接続し直してから問い合わせる)
        assert_eq!(w.poll(&mut port, &t), WatchPoll::Idle);
        assert_eq!(w.poll(&mut port, &t), WatchPoll::Idle);
        assert_eq!(w.poll(&mut port, &t), WatchPoll::Finger);
        assert_eq!(port.sent.iter().filter(|p| p[3] == CMD_CONNECTION).count(), 2);
    }

    #[test]
    fn watch_unsupported_only_on_01_10() {
        let (out, _) = watch_run(
            vec![
                connected(),
                Some(reply(CMD_FINGER_STATE, &[XG_ERR_FAIL, XG_ERR_NO_SUPPORT])),
            ],
            1,
        );
        assert_eq!(out, vec![WatchPoll::Unsupported]);
    }

    #[test]
    fn watch_transient_failures_back_off_then_recover() {
        let mut port = FakePort::new(vec![
            None, // 接続に応答なし
            None,
            None,
            connected(),
            finger(Some(0)),
        ]);
        let t = Timeouts::DEFAULT;
        let mut w = FingerWatch::new();
        assert_eq!(w.interval_ms(), WATCH_INTERVAL_MS);
        for _ in 0..WATCH_BACKOFF_AFTER {
            assert_eq!(w.poll(&mut port, &t), WatchPoll::Idle);
        }
        assert!(w.is_failing());
        assert_eq!(w.interval_ms(), WATCH_BACKOFF_MS);
        // 戻ったら成功で間隔も戻る
        assert_eq!(w.poll(&mut port, &t), WatchPoll::Idle);
        assert!(!w.is_failing());
        assert_eq!(w.interval_ms(), WATCH_INTERVAL_MS);
    }

    #[test]
    fn watch_failed_reply_reconnects_and_keeps_watching() {
        // 時間切れ・壊れた応答・想定外のデータは止めずに接続し直す
        let (out, port) = watch_run(
            vec![
                connected(),
                None,
                connected(),
                finger(Some(9)),
                connected(),
                finger(Some(0)),
                finger(Some(1)),
            ],
            4,
        );
        use WatchPoll::*;
        assert_eq!(out, vec![Idle, Idle, Idle, Finger]);
        assert_eq!(port.sent.iter().filter(|p| p[3] == CMD_CONNECTION).count(), 3);
    }

    #[test]
    fn command_matches_spec_example() {
        assert_eq!(command(CMD_CONNECTION, DEFAULT_PASSWORD), SPEC_CONNECT_SEND);
    }

    #[test]
    fn command_truncates_data_over_16() {
        let p = command(0x55, &[1u8; 20]);
        assert_eq!(p[5], 16);
        assert_eq!(&p[6..22], &[1u8; 16]);
        assert_eq!(parse_packet(&p).unwrap().cmd, 0x55);
    }

    #[test]
    fn checksum_wraps_at_16_bits() {
        assert_eq!(checksum(&[0xFF; 300]), (0xFFu32 * 300 % 0x10000) as u16);
        assert_eq!(checksum(&[]), 0);
    }

    #[test]
    fn parse_packet_accepts_spec_reply() {
        let p = parse_packet(&SPEC_CONNECT_RECV).unwrap();
        assert_eq!(p.cmd, CMD_CONNECTION);
        assert_eq!(p.data[0], XG_ERR_SUCCESS);
        assert_eq!(p.data[15], 0x02);
    }

    #[test]
    fn parse_packet_rejects_bad_prefix_and_checksum() {
        let mut bad = SPEC_CONNECT_RECV;
        bad[0] = 0xAA;
        assert_eq!(parse_packet(&bad), Err(PacketError::Prefix));
        let mut bad = SPEC_CONNECT_RECV;
        bad[10] ^= 1;
        assert_eq!(parse_packet(&bad), Err(PacketError::Checksum));
    }

    #[test]
    fn read_data_request_layout() {
        let p = read_data_request(CMD_READ_DATA, CMD_READ_ENROLL, 0x200, 0x48);
        assert_eq!(p[3], CMD_READ_DATA);
        assert_eq!(p[5], 9);
        assert_eq!(&p[6..15], &[0x22, 0x00, 0x02, 0, 0, 0x48, 0x00, 0, 0]);
        assert!(parse_packet(&p).is_ok());
        // WRITE_DATA は同じ並びで、種別が WRITE_ENROLL (0x23)
        let p = read_data_request(CMD_WRITE_DATA, CMD_WRITE_ENROLL, 0x200, 0x48);
        assert_eq!(p[3], CMD_WRITE_DATA);
        assert_eq!(&p[6..15], &[0x23, 0x00, 0x02, 0, 0, 0x48, 0x00, 0, 0]);
    }

    #[test]
    fn verify_chunk_checks_sum_and_length() {
        let c = chunk(&[1, 2, 3]);
        assert_eq!(verify_chunk(&c, 3), Some(&[1u8, 2, 3][..]));
        assert_eq!(verify_chunk(&c[..4], 3), None);
        let mut bad = c.clone();
        bad[4] ^= 1;
        assert_eq!(verify_chunk(&bad, 3), None);
    }

    #[test]
    fn chunks_split_by_512() {
        let v: Vec<_> = chunks(0x448).collect();
        assert_eq!(v, vec![(0, 512), (512, 512), (1024, 72)]);
        assert_eq!(chunks(512).collect::<Vec<_>>(), vec![(0, 512)]);
        assert_eq!(chunks(0).count(), 0);
        // 実機の大きさ (0x1FDC) は 16 塊、最後は 476 バイト
        let real: Vec<_> = chunks(0x1FDC).collect();
        assert_eq!(real.len(), 16);
        assert_eq!(real[15], (15 * 512, 0x1FDC - 15 * 512));
    }

    #[test]
    fn enroll_reply_kinds() {
        let pk = |d: &[u8]| parse_packet(&reply(CMD_ENROLL, d).try_into().unwrap()).unwrap();
        assert_eq!(enroll_reply(&pk(&[0x00, 1, 2])), EnrollReply::Done);
        assert_eq!(
            enroll_reply(&pk(&[XG_INPUT_FINGER])),
            EnrollReply::Prompt(Prompt::Place)
        );
        assert_eq!(
            enroll_reply(&pk(&[XG_RELEASE_FINGER])),
            EnrollReply::Prompt(Prompt::Release)
        );
        assert_eq!(
            enroll_reply(&pk(&[XG_ERR_FAIL, 0x0B])),
            EnrollReply::Failed(0x0B)
        );
        assert_eq!(enroll_reply(&pk(&[0x42])), EnrollReply::Unknown(0x42));
        // 撮れなかった / 違う指は終わりではなく撮り直し (§2.2.20)
        assert_eq!(
            enroll_reply(&pk(&[XG_ERR_FAIL, XG_ERR_NO_VEIN])),
            EnrollReply::Prompt(Prompt::Retry(XG_ERR_NO_VEIN))
        );
        assert_eq!(
            enroll_reply(&pk(&[XG_ERR_FAIL, XG_ERR_NO_SAME_FINGER])),
            EnrollReply::Prompt(Prompt::Retry(XG_ERR_NO_SAME_FINGER))
        );
    }

    #[test]
    fn reasons_and_lines() {
        assert_eq!(err_line(VeinError::NoModule), "ERR VEIN NO_MODULE");
        assert_eq!(err_line(VeinError::Timeout), "ERR VEIN TIMEOUT");
        assert_eq!(err_line(VeinError::NoFinger), "ERR VEIN NO_FINGER");
        assert_eq!(err_line(VeinError::ReadFail), "ERR VEIN READ_FAIL");
        assert_eq!(err_line(VeinError::Rc(0x0C)), "ERR VEIN RC=0C");
        assert_eq!(chara_line(&[0x0A, 0xBD]), "VEIN CHARA 0ABD");
        assert_eq!(chara_line(&sample_chara(0x1FDC)).len(), 11 + 2 * 0x1FDC);
    }

    #[test]
    fn voice_parse_and_label() {
        for v in [
            VeinVoice::Place,
            VeinVoice::Again,
            VeinVoice::Enrolled,
            VeinVoice::Failed,
        ] {
            assert_eq!(VeinVoice::parse(v.label()), Some(v));
            assert_eq!(VeinVoice::parse(&v.label().to_ascii_lowercase()), Some(v));
        }
        assert_eq!(VeinVoice::parse("HELLO"), None);
        assert_eq!(say_ok_line(VeinVoice::Enrolled), "OK VEIN SAY ENROLLED");
    }

    #[test]
    fn capture_happy_path() {
        let chara = sample_chara(0x448);
        let mut script = head(chara.len());
        for (off, len) in chunks(chara.len()) {
            script.push(Some(chunk(&chara[off..off + len])));
        }
        script.push(cleared());
        let mut port = FakePort::new(script);
        let mut prompts = Vec::new();
        let got = capture(&mut port, &Timeouts::DEFAULT, &mut |p| prompts.push(p));
        assert_eq!(got, Ok(chara));
        assert_eq!(prompts, vec![Prompt::Place, Prompt::Release]);
        // 接続 → 削除 → 登録 → 大きさ → READ_DATA ×3 → 削除
        assert_eq!(port.sent.len(), 8);
        assert_eq!(port.sent[0], SPEC_CONNECT_SEND.to_vec());
        assert_eq!(&port.sent[1][3..10], &[CMD_CLEAR_ENROLL, 0, 4, 1, 0, 0, 0]);
        // 登録: ID 1、テンプレート 1、撮り直し 2 (仕様書の例と同じ 12 バイト)
        assert_eq!(
            &port.sent[2][3..18],
            &[CMD_ENROLL, 0, 12, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 2, 0]
        );
        assert_eq!(&port.sent[3][3..10], &[CMD_READ_ENROLL, 0, 4, 1, 0, 0, 0]);
        assert_eq!(
            &port.sent[6][6..15],
            &[0x22, 0x00, 0x04, 0, 0, 0x48, 0, 0, 0]
        );
        assert_eq!(port.sent[7][3], CMD_CLEAR_ENROLL);
    }

    #[test]
    fn capture_ok_even_if_final_clear_is_silent() {
        let data = vec![0xDE, 0xED, 1, 2];
        let mut script = head(4);
        script.push(Some(chunk(&data)));
        script.push(None);
        let mut port = FakePort::new(script);
        assert_eq!(run(&mut port), Ok(data));
    }

    #[test]
    fn capture_skips_garbage_before_prefix() {
        let data = vec![0xDE, 0xED, 1, 2];
        let mut noisy = vec![0x00, 0xBB, 0x13];
        noisy.extend(SPEC_CONNECT_RECV);
        let mut script = head(4);
        script[0] = Some(noisy);
        script.push(Some(chunk(&data)));
        let mut port = FakePort::new(script);
        assert_eq!(run(&mut port), Ok(data));
    }

    #[test]
    fn capture_retries_bad_chunk() {
        let data = vec![9u8; 10];
        let mut bad = chunk(&data);
        bad[0] ^= 0xFF;
        let mut script = head(10);
        script.extend([Some(bad), None, Some(chunk(&data)), cleared()]);
        let mut port = FakePort::new(script);
        assert_eq!(run(&mut port), Ok(data));
        // 送る前に毎回残りを捨てる (接続・削除・登録・大きさ・塊 3 回・削除)
        assert_eq!(port.discards, 8);
    }

    #[test]
    fn capture_gives_up_after_retries() {
        let mut script = head(10);
        script.extend([None, None, None]);
        let mut port = FakePort::new(script);
        assert_eq!(run(&mut port), Err(VeinError::ReadFail));
        assert_eq!(port.sent.len(), 4 + READ_RETRIES);
    }

    #[test]
    fn capture_chunk_cut_short_is_retried_then_fails() {
        let short = chunk(&[1u8; 10])[..5].to_vec();
        let mut script = head(10);
        script.extend([Some(short.clone()), Some(short.clone()), Some(short)]);
        let mut port = FakePort::new(script);
        assert_eq!(run(&mut port), Err(VeinError::ReadFail));
    }

    #[test]
    fn capture_read_data_send_fails_is_retried() {
        let data = vec![5u8; 3];
        let mut script = head(3);
        script.extend([None, Some(chunk(&data)), cleared()]);
        let mut port = FailAt(FakePort::new(script), 5);
        assert_eq!(run(&mut port), Ok(data));
    }

    #[test]
    fn capture_no_module_when_silent() {
        let mut port = FakePort::new(vec![None]);
        assert_eq!(run(&mut port), Err(VeinError::NoModule));
    }

    #[test]
    fn capture_no_module_when_send_fails() {
        let mut port = FakePort::new(vec![]);
        port.send_ok = false;
        assert_eq!(run(&mut port), Err(VeinError::NoModule));
    }

    #[test]
    fn capture_connect_errors() {
        let one = |r: Vec<u8>| run(&mut FakePort::new(vec![Some(r)]));
        // 途中で途絶えた接続応答
        assert_eq!(one(SPEC_CONNECT_RECV[..10].to_vec()), Err(VeinError::ReadFail));
        // 別コマンドの応答
        assert_eq!(one(reply(0x02, &[0])), Err(VeinError::ReadFail));
        // パスワード違い (XG_ERR_INVALID_PWD = 0x04)
        assert_eq!(
            one(reply(CMD_CONNECTION, &[XG_ERR_FAIL, 0x04])),
            Err(VeinError::Rc(0x04))
        );
        // 和が合わない
        let mut bad = SPEC_CONNECT_RECV.to_vec();
        bad[8] ^= 1;
        assert_eq!(one(bad), Err(VeinError::ReadFail));
        // 1 バイトだけ届いて途絶えた (識別子の 2 バイト目が来ない)
        assert_eq!(one(vec![0xBB]), Err(VeinError::ReadFail));
        // 識別子が見つからないまま前置きが長すぎる
        assert_eq!(one(vec![0x11; MAX_GARBAGE + 3]), Err(VeinError::ReadFail));
    }

    #[test]
    fn capture_clear_errors() {
        let two = |r: Option<Vec<u8>>| run(&mut FakePort::new(vec![connected(), r]));
        assert_eq!(two(None), Err(VeinError::Timeout));
        assert_eq!(two(Some(reply(CMD_CONNECTION, &[0]))), Err(VeinError::ReadFail));
        assert_eq!(two(Some(SPEC_CONNECT_RECV[..5].to_vec())), Err(VeinError::ReadFail));
        let mut port = FailAt(FakePort::new(vec![connected()]), 2);
        assert_eq!(run(&mut port), Err(VeinError::ReadFail));
    }

    #[test]
    fn capture_enroll_errors() {
        let three = |r: Option<Vec<u8>>| run(&mut FakePort::new(vec![connected(), cleared(), r]));
        assert_eq!(three(None), Err(VeinError::Timeout));
        assert_eq!(
            three(Some(reply(CMD_ENROLL, &[XG_ERR_FAIL, XG_ERR_TIME_OUT]))),
            Err(VeinError::NoFinger)
        );
        assert_eq!(
            three(Some(reply(CMD_ENROLL, &[XG_ERR_FAIL, 0x0D]))),
            Err(VeinError::Rc(0x0D))
        );
        assert_eq!(three(Some(reply(CMD_ENROLL, &[0x42]))), Err(VeinError::Rc(0x42)));
        assert_eq!(three(Some(reply(CMD_CONNECTION, &[0]))), Err(VeinError::ReadFail));
        assert_eq!(three(Some(SPEC_CONNECT_RECV[..3].to_vec())), Err(VeinError::ReadFail));
        let mut port = FailAt(FakePort::new(vec![connected(), cleared()]), 3);
        assert_eq!(run(&mut port), Err(VeinError::ReadFail));
    }

    #[test]
    fn capture_retries_after_no_vein() {
        // 置く → 撮れない (01 11) → 置き直す → 離して → 完了
        let mut e = reply(CMD_ENROLL, &[XG_INPUT_FINGER]);
        e.extend(reply(CMD_ENROLL, &[XG_ERR_FAIL, XG_ERR_NO_VEIN]));
        e.extend(reply(CMD_ENROLL, &[XG_INPUT_FINGER]));
        e.extend(reply(CMD_ENROLL, &[XG_RELEASE_FINGER]));
        e.extend(reply(CMD_ENROLL, &[XG_ERR_SUCCESS, 1]));
        let data = vec![0xDE, 0xED, 1, 2];
        let mut port = FakePort::new(vec![connected(), cleared(), Some(e), size(4), Some(chunk(&data)), cleared()]);
        let mut prompts = Vec::new();
        assert_eq!(capture(&mut port, &Timeouts::DEFAULT, &mut |p| prompts.push(p)), Ok(data));
        assert_eq!(
            prompts,
            vec![Prompt::Place, Prompt::Retry(XG_ERR_NO_VEIN), Prompt::Place, Prompt::Release]
        );
    }

    #[test]
    fn production_enroll_counts_retries_per_placement() {
        // 1 枚目で上限ちょうどの撮り直し → 撮れる (離して) → 2 枚目でも上限ちょうど → 撮れる → 完了。
        // 全体で数えていたら 2 枚目の途中で RC になっていた。1 枚目と 2 枚目の間の「置いて」は置き直しの案内
        let mut e = Vec::new();
        for _ in 0..2 {
            for _ in 0..ENROLL_PLACEMENT_RETRIES {
                e.extend(reply(CMD_ENROLL, &[XG_ERR_FAIL, XG_ERR_NO_VEIN]));
            }
            e.extend(reply(CMD_ENROLL, &[XG_RELEASE_FINGER]));
        }
        e.extend(reply(CMD_ENROLL, &[XG_ERR_SUCCESS, 2]));
        let mut port = FakePort::new(vec![cleared(), Some(e)]);
        let mut retries = 0;
        let got = enroll_id(&mut port, &Timeouts::DEFAULT, 10, 2, &mut |p| {
            if matches!(p, Prompt::Retry(_)) {
                retries += 1;
            }
        });
        assert_eq!(got, Ok(()));
        assert_eq!(retries, 2 * usize::from(ENROLL_PLACEMENT_RETRIES));
        // 本番の登録は bData[10] に 1 回置くごとの回数を渡す
        assert_eq!(port.sent[1][6 + 10], ENROLL_PLACEMENT_RETRIES);
    }

    #[test]
    fn production_enroll_gives_up_when_one_placement_exceeds_the_retries() {
        let e: Vec<u8> = (0..=ENROLL_PLACEMENT_RETRIES)
            .flat_map(|_| reply(CMD_ENROLL, &[XG_ERR_FAIL, XG_ERR_NO_VEIN]))
            .collect();
        let mut port = FakePort::new(vec![cleared(), Some(e)]);
        let got = enroll_id(&mut port, &Timeouts::DEFAULT, 10, 2, &mut |_| {});
        assert_eq!(got, Err(VeinError::Rc(XG_ERR_NO_VEIN)));
    }

    #[test]
    fn capture_gives_up_after_too_many_retries() {
        let e: Vec<u8> = (0..=ENROLL_RETRIES)
            .flat_map(|_| reply(CMD_ENROLL, &[XG_ERR_FAIL, XG_ERR_NO_SAME_FINGER]))
            .collect();
        let mut port = FakePort::new(vec![connected(), cleared(), Some(e)]);
        let mut n = 0;
        let got = capture(&mut port, &Timeouts::DEFAULT, &mut |_| n += 1);
        assert_eq!(got, Err(VeinError::Rc(XG_ERR_NO_SAME_FINGER)));
        assert_eq!(n, usize::from(ENROLL_RETRIES));
    }

    #[test]
    fn capture_endless_prompts_is_read_fail() {
        let prompts: Vec<u8> = (0..=MAX_PROMPTS)
            .flat_map(|_| reply(CMD_ENROLL, &[XG_INPUT_FINGER]))
            .collect();
        let mut port = FakePort::new(vec![connected(), cleared(), Some(prompts)]);
        let mut n = 0;
        let got = capture(&mut port, &Timeouts::DEFAULT, &mut |_| n += 1);
        assert_eq!(got, Err(VeinError::ReadFail));
        assert_eq!(n, MAX_PROMPTS + 1);
    }

    #[test]
    fn capture_read_enroll_errors() {
        let four = |r: Option<Vec<u8>>| {
            run(&mut FakePort::new(vec![connected(), cleared(), enrolled(), r]))
        };
        assert_eq!(
            four(Some(reply(CMD_READ_ENROLL, &[XG_ERR_FAIL, 0x07]))),
            Err(VeinError::Rc(0x07))
        );
        assert_eq!(four(None), Err(VeinError::Timeout));
        // 大きさ 0 / 上限超え
        assert_eq!(four(size(0)), Err(VeinError::ReadFail));
        assert_eq!(four(size(CHARA_MAX + 1)), Err(VeinError::ReadFail));
    }

    /// 合成の 0xBDBD 特徴量 (本物の指のデータは使わない — 公開 repo)。`seed` で模様を変えた
    /// 平面を vein-match-search の `make_entry` で詰め、先頭 0x448 バイトを特徴量とする
    /// (登録データの中の件と同じ書き方なので、テンプレートに包んで取り出すと同じに戻る)
    fn synth_chara(seed: usize) -> [u8; CHARA_LEN] {
        let mut plane = [0u8; vein_match::PLANE_LEN];
        for (i, v) in plane.iter_mut().enumerate() {
            *v = u8::from((i * 31 + seed * 17) % 7 < 3);
        }
        let entry = vein_match_search::make_entry(&plane);
        let mut c = [0u8; CHARA_LEN];
        c.copy_from_slice(&entry[..CHARA_LEN]);
        c
    }

    /// 合成の特徴量からモジュールと同じ包み (mode 1 = AES のみ) の登録データを組む
    fn synth_enroll(charas: &[&[u8]]) -> Vec<u8> {
        vein_match_search::create_template(charas, 0, 0, 1, &[]).unwrap()
    }

    #[test]
    fn extract_chara_returns_the_single_bdbd() {
        let c = synth_chara(1);
        assert!(vein_match::load(&c, CHARA_LEN as u32, &mut [0; vein_match::PLANE_LEN]).is_ok());
        let t = synth_enroll(&[&c]);
        assert_eq!(&t[..4], &[0xDE, 0xED, 0xDE, 0xED]);
        assert_eq!(t[6], 1); // AES の包み
        assert_eq!(extract_chara(&t), Ok(c));
        assert_eq!(chara_line(&c).len(), 11 + 2 * CHARA_LEN);
    }

    #[test]
    fn extract_chara_rejects_two_entries() {
        let t = synth_enroll(&[&synth_chara(1), &synth_chara(2)]);
        assert_eq!(extract_chara(&t), Err(VeinError::ReadFail));
    }

    #[test]
    fn extract_chara_rejects_zero_entries() {
        // 件数 0 の記録 (create_template は 0 件を組まないので、記録を直に作って包む)
        let mut rec = vec![0u8; vein_match_search::REC_SIZE];
        rec[..2].copy_from_slice(&vein_match_search::RECORD_MAGIC.to_le_bytes());
        rec[3] = 2;
        let t = vein_match_search::build_template(&rec, 0, 1, &[]).unwrap();
        assert_eq!(extract_chara(&t), Err(VeinError::ReadFail));
    }

    #[test]
    fn extract_chara_rejects_broken_input() {
        // 外側ヘッダでない (取り出し前の見本の形・空)
        assert_eq!(extract_chara(&sample_chara(0x40)), Err(VeinError::ReadFail));
        assert_eq!(extract_chara(&[]), Err(VeinError::ReadFail));
        // 記録の版違い
        let mut rec = vein_match_search::make_record(&[[0u8; vein_match::PLANE_LEN]], 0, 0).unwrap();
        rec[3] = 1;
        let t = vein_match_search::build_template(&rec, 0, 1, &[]).unwrap();
        assert_eq!(extract_chara(&t), Err(VeinError::ReadFail));
        // 未対応の層 (LZW)
        let mut t = synth_enroll(&[&synth_chara(3)]);
        t[6] = 4;
        assert_eq!(extract_chara(&t), Err(VeinError::ReadFail));
    }

    // ---- 試験用コマンド (VEIN TEST …) ----

    fn run_test_enroll(script: Vec<Option<Vec<u8>>>, id: u32) -> (Result<(), VeinError>, FakePort, Vec<Prompt>) {
        let mut port = FakePort::new(script);
        let mut prompts = Vec::new();
        let r = test_enroll(&mut port, &Timeouts::DEFAULT, id, &mut |p| prompts.push(p));
        (r, port, prompts)
    }

    #[test]
    fn test_id_range() {
        assert_eq!(parse_test_id("2"), Some(2));
        assert_eq!(parse_test_id("9"), Some(9));
        // 1 (作業用)・範囲外・数字でないものは不可
        for bad in ["1", "0", "10", "", "-2", "+2", "2a", "a", " 2", "02x", "99999999999"] {
            assert_eq!(parse_test_id(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn test_enroll_sends_clear_then_enroll_for_the_id_and_leaves_it() {
        let script = vec![connected(), cleared(), enrolled()];
        let (r, port, prompts) = run_test_enroll(script, 3);
        assert_eq!(r, Ok(()));
        assert_eq!(prompts, vec![Prompt::Place, Prompt::Release]);
        // 接続 → 削除(ID 3) → 登録(ID 3)。READ_ENROLL も最後の削除も送らない (モジュールに残す)
        assert_eq!(port.sent.len(), 3);
        assert_eq!(port.sent[0], SPEC_CONNECT_SEND.to_vec());
        assert_eq!(&port.sent[1][3..10], &[CMD_CLEAR_ENROLL, 0, 4, 3, 0, 0, 0]);
        assert_eq!(
            &port.sent[2][3..18],
            &[CMD_ENROLL, 0, 12, 3, 0, 0, 0, 0, 1, 0, 0, 0, 0, 2, 0]
        );
    }

    #[test]
    fn test_enroll_errors() {
        // モジュール無し
        let (r, _, _) = run_test_enroll(vec![None], 2);
        assert_eq!(r, Err(VeinError::NoModule));
        // 指が置かれず時間切れ
        let timeout = Some(reply(CMD_ENROLL, &[XG_ERR_FAIL, XG_ERR_TIME_OUT]));
        let (r, _, _) = run_test_enroll(vec![connected(), cleared(), timeout], 2);
        assert_eq!(r, Err(VeinError::NoFinger));
        // その他の失敗コード
        let fail = Some(reply(CMD_ENROLL, &[XG_ERR_FAIL, 0x07]));
        let (r, _, _) = run_test_enroll(vec![connected(), cleared(), fail], 2);
        assert_eq!(r, Err(VeinError::Rc(0x07)));
        // 削除に応答が無い
        let (r, _, _) = run_test_enroll(vec![connected(), None], 2);
        assert_eq!(r, Err(VeinError::Timeout));
    }

    fn run_test_verify(
        script: Vec<Option<Vec<u8>>>,
        id: u32,
    ) -> (Result<[u8; DATA_LEN], VeinError>, FakePort, Vec<Prompt>) {
        let mut port = FakePort::new(script);
        let mut prompts = Vec::new();
        let r = test_verify(&mut port, &Timeouts::DEFAULT, id, &mut |p| prompts.push(p));
        (r, port, prompts)
    }

    #[test]
    fn test_verify_success_returns_final_reply_after_prompts() {
        let mut v = reply(CMD_VERIFY, &[XG_INPUT_FINGER]);
        v.extend(reply(CMD_VERIFY, &[XG_RELEASE_FINGER]));
        v.extend(reply(CMD_VERIFY, &[XG_ERR_SUCCESS, 5, 0x12, 0x34]));
        let (r, port, prompts) = run_test_verify(vec![connected(), Some(v)], 2);
        let data = r.unwrap();
        assert_eq!(&data[..4], &[XG_ERR_SUCCESS, 5, 0x12, 0x34]);
        assert_eq!(prompts, vec![Prompt::Place, Prompt::Release]);
        // 接続 → VERIFY(ID 2、引数は ID の LE 4 バイトだけ)
        assert_eq!(port.sent.len(), 2);
        assert_eq!(&port.sent[1][3..10], &[CMD_VERIFY, 0, 4, 2, 0, 0, 0]);
    }

    #[test]
    fn test_verify_failure_codes_are_returned_not_errors() {
        // 不一致などの失敗コード (01 xx) は最終応答としてそのまま返す (RC=01 で出る)
        let fail = Some(reply(CMD_VERIFY, &[XG_ERR_FAIL, 0x08]));
        let (r, _, _) = run_test_verify(vec![connected(), fail], 2);
        assert_eq!(r.unwrap()[..2], [XG_ERR_FAIL, 0x08]);
        // 撮り直しの途中経過(01 11)はモジュールが続ける
        let mut v = reply(CMD_VERIFY, &[XG_ERR_FAIL, XG_ERR_NO_VEIN]);
        v.extend(reply(CMD_VERIFY, &[XG_ERR_SUCCESS]));
        let (r, _, prompts) = run_test_verify(vec![connected(), Some(v)], 2);
        assert!(r.is_ok());
        assert_eq!(prompts, vec![Prompt::Retry(XG_ERR_NO_VEIN)]);
    }

    #[test]
    fn test_verify_unsupported_timeout_and_errors() {
        let unsupported = Some(reply(CMD_VERIFY, &[XG_ERR_FAIL, XG_ERR_NO_SUPPORT]));
        let (r, _, _) = run_test_verify(vec![connected(), unsupported], 2);
        assert_eq!(r, Err(VeinError::Unsupported));
        let timeout = Some(reply(CMD_VERIFY, &[XG_ERR_FAIL, XG_ERR_TIME_OUT]));
        let (r, _, _) = run_test_verify(vec![connected(), timeout], 2);
        assert_eq!(r, Err(VeinError::NoFinger));
        let (r, _, _) = run_test_verify(vec![connected(), None], 2);
        assert_eq!(r, Err(VeinError::Timeout));
        // 別コマンドの応答
        let other = Some(reply(CMD_ENROLL, &[XG_ERR_SUCCESS]));
        let (r, _, _) = run_test_verify(vec![connected(), other], 2);
        assert_eq!(r, Err(VeinError::ReadFail));
        // 途中経過が止まらない
        let endless: Vec<u8> = (0..=MAX_PROMPTS)
            .flat_map(|_| reply(CMD_VERIFY, &[XG_INPUT_FINGER]))
            .collect();
        let (r, _, _) = run_test_verify(vec![connected(), Some(endless)], 2);
        assert_eq!(r, Err(VeinError::ReadFail));
    }

    #[test]
    fn clear_sends_clear_for_the_id() {
        let mut port = FakePort::new(vec![connected(), Some(reply(CMD_CLEAR_ENROLL, &[XG_ERR_SUCCESS]))]);
        assert_eq!(clear(&mut port, &Timeouts::DEFAULT, 4), Ok(()));
        assert_eq!(&port.sent[1][3..10], &[CMD_CLEAR_ENROLL, 0, 4, 4, 0, 0, 0]);
        // 未登録などの失敗コードは Rc で返す
        let mut port = FakePort::new(vec![connected(), cleared()]);
        assert_eq!(clear(&mut port, &Timeouts::DEFAULT, 4), Err(VeinError::Rc(0x07)));
        let mut port = FakePort::new(vec![connected(), None]);
        assert_eq!(clear(&mut port, &Timeouts::DEFAULT, 4), Err(VeinError::Timeout));
        let mut port = FakePort::new(vec![None]);
        assert_eq!(clear(&mut port, &Timeouts::DEFAULT, 4), Err(VeinError::NoModule));
    }

    #[test]
    fn test_lines() {
        assert_eq!(test_enrolled_line(2), "VEIN TEST ENROLLED 2");
        assert_eq!(test_cleared_line(9), "VEIN TEST CLEARED 9");
        assert_eq!(
            test_verify_line(2, &[0x00, 0x05, 0xAB]),
            "VEIN TEST VERIFY 2 RC=00 DATA=05AB"
        );
        assert_eq!(test_verify_line(3, &[0x01]), "VEIN TEST VERIFY 3 RC=01 DATA=");
        // DATA は先頭 32 バイト (64 文字) で切る
        let long: Vec<u8> = (0..100u8).collect();
        let line = test_verify_line(2, &long);
        let data = line.rsplit("DATA=").next().unwrap();
        assert_eq!(data.len(), VERIFY_DATA_MAX * 2);
        assert!(data.starts_with("0102030405"));
        assert_eq!(VeinError::Unsupported.reason(), "UNSUPPORTED");
        assert_eq!(err_line(VeinError::Unsupported), "ERR VEIN UNSUPPORTED");
    }

    fn ack_write(ok: bool) -> Option<Vec<u8>> {
        let rc = if ok { XG_ERR_SUCCESS } else { XG_ERR_FAIL };
        Some(reply(CMD_WRITE_DATA, &[rc]))
    }

    fn write_ready() -> Option<Vec<u8>> {
        Some(reply(CMD_WRITE_ENROLL, &[XG_ERR_SUCCESS]))
    }

    #[test]
    fn test_verify_id_zero_is_one_to_n() {
        // ID 0 = 1:N (§2.2.21)。送る bData は ID の LE 4 バイト (00 00 00 00)、
        // 成功の応答の bData[1..5] (LE) が当たった ID
        let v = reply(CMD_VERIFY, &[XG_ERR_SUCCESS, 7, 0, 0, 0]);
        let (r, port, _) = run_test_verify(vec![connected(), Some(v)], 0);
        let data = r.unwrap();
        assert_eq!(&port.sent[1][3..10], &[CMD_VERIFY, 0, 4, 0, 0, 0, 0]);
        assert_eq!(test_verify_line(0, &data), "VEIN TEST VERIFY 0 RC=00 DATA=070000000000000000000000000000");
    }

    #[test]
    fn test_capacity_reads_users_and_max() {
        let mut d = [0u8; 13];
        d[0] = XG_ERR_SUCCESS;
        d[1..5].copy_from_slice(&2u32.to_le_bytes());
        d[9..13].copy_from_slice(&0x0102u32.to_le_bytes());
        let mut port = FakePort::new(vec![connected(), Some(reply(CMD_GET_ENROLL_INFO, &d))]);
        assert_eq!(test_capacity(&mut port, &Timeouts::DEFAULT), Ok((2, 0x0102)));
        // 接続 → GET_ENROLL_INFO (bData 無し)
        assert_eq!(port.sent.len(), 2);
        assert_eq!(port.sent[1], command(CMD_GET_ENROLL_INFO, &[]).to_vec());
        // 失敗コードは Rc、応答が無ければ Timeout、モジュールが居なければ NoModule
        let mut port = FakePort::new(vec![connected(), Some(reply(CMD_GET_ENROLL_INFO, &[XG_ERR_FAIL, 0x07]))]);
        assert_eq!(test_capacity(&mut port, &Timeouts::DEFAULT), Err(VeinError::Rc(0x07)));
        let mut port = FakePort::new(vec![connected(), None]);
        assert_eq!(test_capacity(&mut port, &Timeouts::DEFAULT), Err(VeinError::Timeout));
        let mut port = FakePort::new(vec![None]);
        assert_eq!(test_capacity(&mut port, &Timeouts::DEFAULT), Err(VeinError::NoModule));
        assert_eq!(test_capacity_line(2, 500), "VEIN TEST CAPACITY USERS=2 MAX=500");
    }

    #[test]
    fn test_image_reads_size_then_pixels_in_chunks() {
        let (w, h) = (40u16, 30u16);
        let img: Vec<u8> = (0..usize::from(w) * usize::from(h)).map(|i| i as u8).collect();
        let mut script = vec![connected()];
        let mut r = reply(CMD_READ_IMAGE, &[XG_INPUT_FINGER]);
        r.extend(reply(CMD_READ_IMAGE, &[XG_ERR_SUCCESS, w as u8, (w >> 8) as u8, h as u8, (h >> 8) as u8]));
        script.push(Some(r));
        for (off, len) in chunks(img.len()) {
            script.push(Some(chunk(&img[off..off + len])));
        }
        let mut port = FakePort::new(script);
        let mut prompts = Vec::new();
        let got = test_image(&mut port, &Timeouts::DEFAULT, &mut |p| prompts.push(p)).unwrap();
        assert_eq!(got, (w, h, img.clone()));
        assert_eq!(prompts, vec![Prompt::Place]);
        // 接続 → READ_IMAGE (bData 6 バイトの 0) → READ_DATA 種別 0x24 ×3 (1200 = 512 + 512 + 176)
        assert_eq!(port.sent[1], command(CMD_READ_IMAGE, &[0u8; 6]).to_vec());
        assert_eq!(port.sent[2], read_data_request(CMD_READ_DATA, CMD_READ_IMAGE, 0, 512).to_vec());
        assert_eq!(port.sent[4], read_data_request(CMD_READ_DATA, CMD_READ_IMAGE, 1024, 176).to_vec());
        assert_eq!(port.sent.len(), 5);

        let lines = image_lines(w, h, &img);
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("VEIN TEST IMAGE 0 000102"));
        assert!(lines[1].starts_with("VEIN TEST IMAGE 1024 "));
        assert_eq!(lines[1].len(), "VEIN TEST IMAGE 1024 ".len() + 176 * 2);
        assert_eq!(lines[2], "VEIN TEST IMAGE END 40 30 1200");
    }

    #[test]
    fn test_image_errors() {
        // 途中経過は数えるだけ (どの台本も 1 回は「置いて」を挟む)
        let mut prompts = 0;
        let mut run = |script: Vec<Option<Vec<u8>>>| {
            let mut port = FakePort::new(script);
            test_image(&mut port, &Timeouts::DEFAULT, &mut |_| prompts += 1)
        };
        let fin = |final_reply: &[u8]| {
            let mut r = reply(CMD_READ_IMAGE, &[XG_INPUT_FINGER]);
            r.extend(reply(CMD_READ_IMAGE, final_reply));
            vec![connected(), Some(r)]
        };
        assert_eq!(run(fin(&[XG_ERR_FAIL, XG_ERR_NO_SUPPORT])), Err(VeinError::Unsupported));
        assert_eq!(run(fin(&[XG_ERR_FAIL, 0x05])), Err(VeinError::Rc(0x05)));
        assert_eq!(run(fin(&[0x42])), Err(VeinError::Rc(0x42)));
        // 幅 × 高さが 0、または上限を超える
        assert_eq!(run(fin(&[XG_ERR_SUCCESS, 0, 0, 240, 0])), Err(VeinError::ReadFail));
        assert_eq!(run(fin(&[XG_ERR_SUCCESS, 0xFF, 0xFF, 0xFF, 0xFF])), Err(VeinError::ReadFail));
        // 塊が読めない
        let mut script = fin(&[XG_ERR_SUCCESS, 4, 0, 1, 0]);
        script.extend([None, None, None]);
        assert_eq!(run(script), Err(VeinError::ReadFail));
        // 応答が無い / モジュールが居ない
        assert_eq!(run(vec![connected(), None]), Err(VeinError::Timeout));
        assert_eq!(run(vec![None]), Err(VeinError::NoModule));
        assert_eq!(prompts, 6);
    }

    #[test]
    fn test_raw_sends_bytes_verbatim_and_returns_whatever_comes_back() {
        // 応答は 7 バイトずつ分けて届く (FakePort)。全部つないで返す
        let frame = vec![0xEF, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0x01, 0x00, 0x03, 0x01, 0x00, 0x05];
        let answer: Vec<u8> = (0u8..20).collect();
        let mut port = FakePort::new(vec![Some(answer.clone())]);
        let one = [frame.clone()];
        assert_eq!(test_raw(&mut port, &Timeouts::DEFAULT, 1000, &one, 20), Ok(answer.clone()));
        // 接続もせず、送ったのはそのままの 1 回だけ。枠が 1 つなら間隔は置かない
        assert_eq!(port.sent, vec![frame.clone()]);
        assert_eq!(port.discards, 1);
        assert_eq!(port.pauses, Vec::<u32>::new());
        // 枠が複数なら、枠の間にだけ間隔を置いて順に送る。0 なら置かない
        let three = [vec![1], vec![2, 3], vec![4]];
        let mut port = FakePort::new(vec![]);
        assert_eq!(test_raw(&mut port, &Timeouts::DEFAULT, 0, &three, 15), Ok(vec![]));
        assert_eq!(port.sent, three.to_vec());
        assert_eq!(port.pauses, vec![15, 15]);
        let mut port = FakePort::new(vec![]);
        assert_eq!(test_raw(&mut port, &Timeouts::DEFAULT, 0, &three, 0), Ok(vec![]));
        assert_eq!(port.pauses, Vec::<u32>::new());
        assert_eq!(raw_gap_line(15), "VEIN TEST RAWGAP 15");
        assert_eq!(test_raw_line(&answer[..3]), "VEIN TEST RAW RX=000102");
        // 何も返らなければ空
        let mut port = FakePort::new(vec![None]);
        assert_eq!(test_raw(&mut port, &Timeouts::DEFAULT, 1000, &one, 0), Ok(vec![]));
        assert_eq!(test_raw_line(&[]), "VEIN TEST RAW RX=");
        // 長い応答は RECORD_LINE_MAX で打ち切る
        let mut port = FakePort::new(vec![Some(vec![0xAA; RECORD_LINE_MAX + 50])]);
        port.max_per_recv = 300;
        assert_eq!(test_raw(&mut port, &Timeouts::DEFAULT, 1000, &one, 0).unwrap().len(), RECORD_LINE_MAX);
        // 送れなければ ReadFail
        let mut port = FakePort::new(vec![]);
        port.send_ok = false;
        assert_eq!(test_raw(&mut port, &Timeouts::DEFAULT, 1000, &one, 0), Err(VeinError::ReadFail));
    }

    #[test]
    fn raw_queue_joins_then_empties_and_refuses_overflow() {
        let mut q = RawQueue::default();
        assert_eq!(q.push(&[1, 2]), Ok(2));
        assert_eq!(q.push(&[3]), Ok(3));
        assert_eq!(raw_queue_line(3), "VEIN TEST RAWQ 3");
        // RAW の分を後ろにつなぎ、空になる
        assert_eq!(q.take_with(&[9]), vec![vec![1, 2], vec![3], vec![9]]);
        assert_eq!(q.take_with(&[]), vec![Vec::<u8>::new()]);
        // 上限ちょうどは入る。超えたら全部捨てる
        assert_eq!(q.push(&vec![0; RAW_QUEUE_MAX]), Ok(RAW_QUEUE_MAX));
        assert_eq!(
            q.push(&[0]),
            Err(format!("ERR VEIN: RAWQ は合わせて {RAW_QUEUE_MAX} バイトまでです (ためた分は捨てた)"))
        );
        assert_eq!(q.take_with(&[7]), vec![vec![7]]);
    }

    #[test]
    fn test_sysinfo_returns_the_data_part_as_is() {
        let d = [XG_ERR_SUCCESS, 0x12, 0x34, 0x02, 0x00, 0xAB];
        let mut port = FakePort::new(vec![connected(), Some(reply(CMD_GET_SYSTEM_INFO, &d))]);
        let got = test_sysinfo(&mut port, &Timeouts::DEFAULT).unwrap();
        assert_eq!(&got[..6], &d);
        // 接続 → GET_SYSTEM_INFO (bData 無し)
        assert_eq!(port.sent.len(), 2);
        assert_eq!(port.sent[1], command(CMD_GET_SYSTEM_INFO, &[]).to_vec());
        assert_eq!(
            test_sysinfo_line(&got),
            format!("VEIN TEST SYSINFO RC=00 DATA=12340200AB{}", "00".repeat(DATA_LEN - 6))
        );
        // 失敗コードも解釈せずに返す。非対応だけは Unsupported、応答が無ければ Timeout、モジュールが居なければ NoModule
        let mut port = FakePort::new(vec![connected(), Some(reply(CMD_GET_SYSTEM_INFO, &[XG_ERR_FAIL, 0x05]))]);
        assert_eq!(&test_sysinfo(&mut port, &Timeouts::DEFAULT).unwrap()[..2], &[XG_ERR_FAIL, 0x05]);
        let mut port = FakePort::new(vec![connected(), Some(reply(CMD_GET_SYSTEM_INFO, &[XG_ERR_FAIL, XG_ERR_NO_SUPPORT]))]);
        assert_eq!(test_sysinfo(&mut port, &Timeouts::DEFAULT), Err(VeinError::Unsupported));
        let mut port = FakePort::new(vec![connected(), None]);
        assert_eq!(test_sysinfo(&mut port, &Timeouts::DEFAULT), Err(VeinError::Timeout));
        let mut port = FakePort::new(vec![None]);
        assert_eq!(test_sysinfo(&mut port, &Timeouts::DEFAULT), Err(VeinError::NoModule));
    }

    fn chunk_cmd(id: u32, offset: usize, data: &[u8]) -> WriteCommand {
        WriteCommand::Chunk { id, offset, data: data.to_vec() }
    }

    fn write_t(slot: &mut Option<TemplateAssembler>, port: &mut dyn Port, cmd: WriteCommand) -> Result<String, VeinError> {
        sync_write(slot, port, &Timeouts::DEFAULT, cmd)
    }

    #[test]
    fn sync_write_assembles_lines_then_writes_on_end() {
        let data = sample_chara(300);
        let mut slot = None;
        let mut port = FakePort::new(vec![connected(), write_ready(), ack_write(true)]);
        assert_eq!(
            write_t(&mut slot, &mut port, chunk_cmd(5, 0, &data[..192])).as_deref(),
            Ok("VEIN SYNC WRITE 5 192")
        );
        assert_eq!(
            write_t(&mut slot, &mut port, chunk_cmd(5, 192, &data[192..])).as_deref(),
            Ok("VEIN SYNC WRITE 5 300")
        );
        // 塊の間はモジュールに触らない
        assert!(port.sent.is_empty());
        assert_eq!(
            write_t(&mut slot, &mut port, WriteCommand::End(5)).as_deref(),
            Ok("VEIN SYNC WRITTEN 5 300")
        );
        assert!(slot.is_none());
        assert_eq!(
            port.sent[1],
            command(CMD_WRITE_ENROLL, &[5, 0, 0, 0, 0x2C, 0x01, 0, 0]).to_vec()
        );
        let mut want = read_data_request(CMD_WRITE_DATA, CMD_WRITE_ENROLL, 0, 300).to_vec();
        want.extend_from_slice(&data);
        want.extend_from_slice(&checksum(&data).to_le_bytes());
        assert_eq!(port.sent[2], want);
    }

    #[test]
    fn sync_write_sequence_errors_discard_the_buffer() {
        let magic = TEMPLATE_MAGIC;
        let mut port = FakePort::new(vec![]);
        // 飛び・重なり・最初が 0 でない・END だけ・別の ID の END は WRITE_SEQ で、組み立て中を捨てる
        let mut slot = None;
        write_t(&mut slot, &mut port, chunk_cmd(5, 0, &magic)).unwrap();
        assert_eq!(write_t(&mut slot, &mut port, chunk_cmd(5, 8, &magic)), Err(VeinError::WriteSeq));
        assert!(slot.is_none());
        write_t(&mut slot, &mut port, chunk_cmd(5, 0, &magic)).unwrap();
        assert_eq!(write_t(&mut slot, &mut port, chunk_cmd(5, 2, &magic)), Err(VeinError::WriteSeq));
        assert_eq!(write_t(&mut slot, &mut port, chunk_cmd(5, 4, &magic)), Err(VeinError::WriteSeq));
        assert_eq!(write_t(&mut slot, &mut port, WriteCommand::End(5)), Err(VeinError::WriteSeq));
        write_t(&mut slot, &mut port, chunk_cmd(5, 0, &magic)).unwrap();
        assert_eq!(write_t(&mut slot, &mut port, chunk_cmd(6, 4, &magic)), Err(VeinError::WriteSeq));
        write_t(&mut slot, &mut port, chunk_cmd(5, 0, &magic)).unwrap();
        assert_eq!(write_t(&mut slot, &mut port, WriteCommand::End(6)), Err(VeinError::WriteSeq));
        // offset 0 は組み立てを最初からやり直す (別の ID でも)
        write_t(&mut slot, &mut port, chunk_cmd(5, 0, &magic)).unwrap();
        assert_eq!(
            write_t(&mut slot, &mut port, chunk_cmd(6, 0, &[1, 2])).as_deref(),
            Ok("VEIN SYNC WRITE 6 2")
        );
        assert_eq!(slot.as_ref().map(|a| a.id), Some(6));
        assert!(port.sent.is_empty());
    }

    #[test]
    fn sync_write_limits_size_and_checks_header() {
        let mut port = FakePort::new(vec![]);
        let mut slot = None;
        // CHARA_MAX ちょうどまでは受け、1 バイトでも超えたら WRITE_TOO_BIG
        let block = vec![0u8; WRITE_LINE_MAX];
        let mut offset = 0;
        while offset + WRITE_LINE_MAX <= CHARA_MAX {
            write_t(&mut slot, &mut port, chunk_cmd(5, offset, &block)).unwrap();
            offset += WRITE_LINE_MAX;
        }
        let rest = CHARA_MAX - offset;
        assert_eq!(
            write_t(&mut slot, &mut port, chunk_cmd(5, offset, &vec![0u8; rest])).as_deref(),
            Ok(format!("VEIN SYNC WRITE 5 {CHARA_MAX}").as_str())
        );
        assert_eq!(
            write_t(&mut slot, &mut port, chunk_cmd(5, CHARA_MAX, &[0])),
            Err(VeinError::WriteTooBig)
        );
        assert!(slot.is_none());
        // 先頭が DE ED DE ED でなければ書かない (モジュールに触らない) / 短すぎても同じ
        write_t(&mut slot, &mut port, chunk_cmd(5, 0, &[0xDE, 0xED, 0xDE, 0xEE])).unwrap();
        assert_eq!(write_t(&mut slot, &mut port, WriteCommand::End(5)), Err(VeinError::WriteHeader));
        write_t(&mut slot, &mut port, chunk_cmd(5, 0, &[0xDE, 0xED])).unwrap();
        assert_eq!(write_t(&mut slot, &mut port, WriteCommand::End(5)), Err(VeinError::WriteHeader));
        assert!(port.sent.is_empty());
    }

    #[test]
    fn sync_write_end_reports_module_errors() {
        let magic = TEMPLATE_MAGIC;
        let mut slot = None;
        let mut port = FakePort::new(vec![None]);
        write_t(&mut slot, &mut port, chunk_cmd(5, 0, &magic)).unwrap();
        assert_eq!(write_t(&mut slot, &mut port, WriteCommand::End(5)), Err(VeinError::NoModule));
        assert!(slot.is_none());
        let mut port = FakePort::new(vec![connected(), Some(reply(CMD_WRITE_ENROLL, &[XG_ERR_FAIL, 0x04]))]);
        write_t(&mut slot, &mut port, chunk_cmd(5, 0, &magic)).unwrap();
        assert_eq!(write_t(&mut slot, &mut port, WriteCommand::End(5)), Err(VeinError::Rc(0x04)));
    }

    #[test]
    fn write_command_debug_hides_the_data() {
        let c = chunk_cmd(5, 192, &[0xDE, 0xED, 0xCA, 0xFE]);
        let text = format!("{c:?} {:?}", WriteCommand::End(5));
        assert_eq!(text, "Chunk(id=5, offset=192, len=4) End(id=5)");
        assert_eq!(WRITE_LINE_MAX * 2, 384);
    }

    #[test]
    fn write_errors_have_reasons() {
        assert_eq!(err_line(VeinError::WriteSeq), "ERR VEIN WRITE_SEQ");
        assert_eq!(err_line(VeinError::WriteTooBig), "ERR VEIN WRITE_TOO_BIG");
        assert_eq!(err_line(VeinError::WriteHeader), "ERR VEIN WRITE_HEADER");
    }

    // ---- 本番の口 (VEIN ENROLL / IDENTIFY / SYNC) ----

    /// VERIFY 1 回の送信に続けて届く応答: 置いて → 離して → 最終 (当たれば ID、`None` は不一致 01 0C)
    fn verified(hit: Option<u32>) -> Option<Vec<u8>> {
        let mut v = reply(CMD_VERIFY, &[XG_INPUT_FINGER]);
        v.extend(reply(CMD_VERIFY, &[XG_RELEASE_FINGER]));
        let last = match hit {
            Some(id) => {
                let mut d = vec![XG_ERR_SUCCESS];
                d.extend_from_slice(&id.to_le_bytes());
                d
            }
            None => vec![XG_ERR_FAIL, XG_ERR_VERIFY],
        };
        v.extend(reply(CMD_VERIFY, &last));
        Some(v)
    }

    fn cleared_ok() -> Option<Vec<u8>> {
        Some(reply(CMD_CLEAR_ENROLL, &[XG_ERR_SUCCESS]))
    }

    fn run_enroll(script: Vec<Option<Vec<u8>>>, id: u32) -> (Result<Vec<u8>, VeinError>, FakePort, Vec<Prompt>) {
        let mut port = FakePort::new(script);
        let mut prompts = Vec::new();
        let r = enroll(&mut port, &Timeouts::DEFAULT, id, &mut |p| prompts.push(p));
        (r, port, prompts)
    }

    #[test]
    fn enroll_takes_the_templates_then_reads_the_record() {
        let data = sample_chara(1100);
        let mut script = vec![connected(), cleared(), enrolled_production(), size(1100)];
        for (off, len) in chunks(1100) {
            script.push(Some(chunk(&data[off..off + len])));
        }
        let (r, port, prompts) = run_enroll(script, 17);
        assert_eq!(r, Ok(data));
        // 撮影: 2 枚目以降の「置いて」は置き直し (2, 3, …)。撮り直しの後の「置いて」は同じ回なのでそのまま。
        // 登録の後の確認の照合はしない
        let n = ENROLL_TEMPLATES_PRODUCTION;
        let mut want = vec![Prompt::Place, Prompt::Release];
        for k in 2..=n {
            want.push(Prompt::Replace(k));
            if k == 2 {
                want.extend([Prompt::Retry(XG_ERR_NO_VEIN), Prompt::Place]);
            }
            want.push(Prompt::Release);
        }
        assert_eq!(prompts, want);
        assert_eq!(ENROLL_PLACEMENTS, n);
        // 接続 → 削除 → 登録 → READ_ENROLL → READ_DATA ×3。最後の削除はしない
        assert_eq!(port.sent.len(), 7);
        assert_eq!(&port.sent[2][3..10], &[CMD_ENROLL, 0, 12, 17, 0, 0, 0]);
        // 本番の登録はテンプレート ENROLL_TEMPLATES_PRODUCTION 本 (bData[5])、撮り直しは 1 回置くごとに ENROLL_PLACEMENT_RETRIES (bData[10])
        assert_eq!(port.sent[2][6 + 5], ENROLL_TEMPLATES_PRODUCTION);
        assert_eq!(port.sent[2][6 + 10], ENROLL_PLACEMENT_RETRIES);
        assert_eq!(&port.sent[3][3..10], &[CMD_READ_ENROLL, 0, 4, 17, 0, 0, 0]);
        assert_eq!(port.pauses, Vec::<u32>::new());
    }

    #[test]
    fn enroll_read_failure_after_registering_clears_the_id() {
        let clear_17 = command(CMD_CLEAR_ENROLL, &[17, 0, 0, 0]).to_vec();
        let script = vec![
            connected(),
            cleared(),
            enrolled(),
            Some(reply(CMD_READ_ENROLL, &[XG_ERR_FAIL, 0x0A])),
            cleared_ok(),
        ];
        let (r, port, _) = run_enroll(script, 17);
        assert_eq!(r, Err(VeinError::Rc(0x0A)));
        assert_eq!(port.sent.last(), Some(&clear_17));
        // 削除が送れなくても、返すのは読み出しの失敗 (5 回目の送信 = 削除)
        let script = vec![connected(), cleared(), enrolled(), Some(reply(CMD_READ_ENROLL, &[XG_ERR_FAIL, 0x0A]))];
        let mut port = FailAt(FakePort::new(script), 5);
        assert_eq!(enroll(&mut port, &Timeouts::DEFAULT, 17, &mut |_| {}), Err(VeinError::Rc(0x0A)));
        assert_eq!(port.0.sent.last(), Some(&clear_17));
    }

    #[test]
    fn enroll_failures_before_registering_do_not_clear_again() {
        let (r, port, _) = run_enroll(vec![None], 17);
        assert_eq!(r, Err(VeinError::NoModule));
        assert_eq!(port.sent.len(), 1);
        let timeout = Some(reply(CMD_ENROLL, &[XG_ERR_FAIL, XG_ERR_TIME_OUT]));
        let (r, port, _) = run_enroll(vec![connected(), cleared(), timeout], 17);
        assert_eq!(r, Err(VeinError::NoFinger));
        assert_eq!(port.sent.len(), 3);
    }

    fn run_identify(script: Vec<Option<Vec<u8>>>) -> (Result<Option<u32>, VeinError>, FakePort) {
        let mut port = FakePort::new(script);
        let r = identify(&mut port, &Timeouts::DEFAULT, &mut |_| {});
        (r, port)
    }

    #[test]
    fn identify_is_verify_zero() {
        let (r, port) = run_identify(vec![connected(), finger(Some(1)), verified(Some(0x0102_0304))]);
        assert_eq!(r, Ok(Some(0x0102_0304)));
        // 接続 → 指の状態 (有る) → 置ききるのを待つ → VERIFY 0
        assert_eq!(port.sent.len(), 3);
        assert_eq!(port.sent[1], command(CMD_FINGER_STATE, &[]).to_vec());
        assert_eq!(port.sent[2], command(CMD_VERIFY, &[0, 0, 0, 0]).to_vec());
        assert_eq!(port.pauses, vec![IDENTIFY_SETTLE_MS]);
        let (r, _) = run_identify(vec![connected(), finger(Some(1)), verified(None)]);
        assert_eq!(r, Ok(None));
        assert_eq!(identify_line(Some(42), None), "VEIN IDENTIFIED 42");
        assert_eq!(identify_line(None, None), "VEIN NOMATCH");
    }

    #[test]
    fn identify_waits_for_the_finger_to_settle() {
        // 無い → 無い → 有る: 問い合わせ間隔で待ち、有るを見てから置ききるのを待つ
        let script = vec![connected(), finger(Some(0)), finger(Some(0)), finger(Some(1)), verified(Some(7))];
        let (r, port) = run_identify(script);
        assert_eq!(r, Ok(Some(7)));
        assert_eq!(port.pauses, vec![SETTLE_POLL_MS, SETTLE_POLL_MS, IDENTIFY_SETTLE_MS]);
        // 非対応・失敗なら待たずに VERIFY (モジュールの指待ちに任せる)
        let unsupported = Some(reply(CMD_FINGER_STATE, &[XG_ERR_FAIL, XG_ERR_NO_SUPPORT]));
        let (r, port) = run_identify(vec![connected(), unsupported, verified(Some(7))]);
        assert_eq!(r, Ok(Some(7)));
        assert_eq!(port.pauses, Vec::<u32>::new());
        let failed = Some(reply(CMD_FINGER_STATE, &[XG_ERR_FAIL, 0x07]));
        let (r, port) = run_identify(vec![connected(), failed, verified(None)]);
        assert_eq!(r, Ok(None));
        assert_eq!(port.pauses, Vec::<u32>::new());
        // 指が来ないまま上限まで問い合わせたら、そのまま VERIFY
        let t = Timeouts { finger_ms: 2 * SETTLE_POLL_MS, ..Timeouts::DEFAULT };
        let mut port = FakePort::new(vec![connected(), finger(Some(0)), finger(Some(0)), finger(Some(0)), verified(None)]);
        assert_eq!(identify(&mut port, &t, &mut |_| {}), Ok(None));
        assert_eq!(port.pauses, vec![SETTLE_POLL_MS; 3]);
        assert_eq!(port.sent.last(), Some(&command(CMD_VERIFY, &[0, 0, 0, 0]).to_vec()));
        // 包んだポート越しでも同じ順 (置ききるのを待ってから VERIFY)
        let mut port = FailAt(FakePort::new(vec![connected(), finger(Some(1)), verified(Some(7))]), 99);
        assert_eq!(identify(&mut port, &Timeouts::DEFAULT, &mut |_| {}), Ok(Some(7)));
        assert_eq!(port.0.pauses, vec![IDENTIFY_SETTLE_MS]);
    }

    // ---- 照合の点数 (動作記録 GET_DEBUG 0x26) ----

    /// 1:1 で FV が通ったときの記録 (古い照合 → 今の照合の順)
    const LOG_FV: &str = "Match.User:15\r\n1:1->A:15, 6,40\r\nPS_Search OK:FV=15,FP=0\r\nUSER:15, th[50, 60, 70]\r\n\
                          Match.User:15\r\n1:1->A:15, 6,66\r\nPS_Search OK:FV=15,FP=15\r\nUSER:15, th[50, 60, 70]\r\n";
    /// FV の点数が 0 で FP が通ったときの記録
    const LOG_FP: &str = "Match.User:15\n1:1->A:15, 6,0\nPS_Search OK:FV=0,FP=15\nUSER:15, th[50, 60, 70]\n\0\0\0";

    #[test]
    fn parse_match_log_reads_the_last_match_of_the_id() {
        let s = parse_match_log(LOG_FV, 15).unwrap();
        assert_eq!(s, MatchScore { score: 66, th: [50, 60, 70], via: MatchVia::Fv });
        // FP で通った (FV の点数は 0)。NUL の埋めがあっても読める
        let s = parse_match_log(LOG_FP, 15).unwrap();
        assert_eq!(s, MatchScore { score: 0, th: [50, 60, 70], via: MatchVia::Fp });
        // 1:N (別の ID の行が混じる) は当たった ID の行だけを見る
        let log = "1:N->A:20, 6,99\nUSER:20, th[1, 2, 3]\nPS_Search OK:FV=20,FP=20\n\
                   1:N->A:15, 6,58\nUSER:15, th[50, 60, 70]\nPS_Search OK:FV=15,FP=0\nUSER:20, th[1, 2, 3]\n";
        assert_eq!(parse_match_log(log, 15).unwrap(), MatchScore { score: 58, th: [50, 60, 70], via: MatchVia::Fv });
        // PS_Search の行が無ければ点数と s0 の比べ方で決める
        let log = "1:1->A:7, 6,50\nUSER:7, th[50, 0, 0]\n";
        assert_eq!(parse_match_log(log, 7).unwrap().via, MatchVia::Fv);
        let log = "1:1->A:7, 6,49\nUSER:7, th[50, 0, 0]\n";
        assert_eq!(parse_match_log(log, 7).unwrap().via, MatchVia::Fp);
        // PS_Search の行が別の ID だけなら (読めないので) 点数で決める
        let log = "1:1->A:7, 6,60\nUSER:7, th[50, 0, 0]\nPS_Search OK:FV=8,FP=-1\n";
        assert_eq!(parse_match_log(log, 7).unwrap().via, MatchVia::Fv);
    }

    #[test]
    fn parse_match_log_gives_none_when_unreadable() {
        assert_eq!(parse_match_log("", 15), None);
        // ID が違う
        assert_eq!(parse_match_log(LOG_FV, 16), None);
        // 点数の行だけ / 閾値の行だけ
        assert_eq!(parse_match_log("1:1->A:15, 6,66\n", 15), None);
        assert_eq!(parse_match_log("USER:15, th[50, 60, 70]\n", 15), None);
        // 点数が数字でない・コンマの後ろが無い
        assert_eq!(parse_match_log("1:1->A:15, 6,xx\nUSER:15, th[1, 2, 3]\n", 15), None);
        assert_eq!(parse_match_log("1:1->A:15\nUSER:15, th[1, 2, 3]\n", 15), None);
        assert_eq!(parse_match_log("1:1->A:x, 6,5\nUSER:15, th[1, 2, 3]\n", 15), None);
        // 閾値が 3 つでない・数字でない・閉じていない・th が無い
        for th in ["th[1, 2]", "th[1, 2, 3, 4]", "th[1, 2, x]", "th[1, 2, 3", "x"] {
            let log = format!("1:1->A:15, 6,5\nUSER:15, {th}\n");
            assert_eq!(parse_match_log(&log, 15), None, "{th}");
        }
        assert_eq!(parse_match_log("1:1->A:15, 6,5\nUSER:x, th[1, 2, 3]\n", 15), None);
    }

    /// GET_DEBUG の応答 (大きさ) と READ_DATA の塊 (512 バイトごと) の台本
    fn debug_script(log: &[u8]) -> Vec<Option<Vec<u8>>> {
        let n = log.len() as u16;
        let mut v = vec![Some(reply(CMD_GET_DEBUG, &[XG_ERR_SUCCESS, n as u8, (n >> 8) as u8]))];
        v.extend(log.chunks(UART_DATA_PACKET_MAX).map(|c| Some(chunk(c))));
        v
    }

    fn with_debug(mut head: Vec<Option<Vec<u8>>>, log: &str) -> Vec<Option<Vec<u8>>> {
        head.extend(debug_script(log.as_bytes()));
        head
    }

    #[test]
    fn identify_and_verify_append_the_score_when_the_log_is_readable() {
        let script = with_debug(vec![connected(), finger(Some(1)), verified(Some(15))], LOG_FV);
        assert_eq!(
            run_t(Command::Identify, script),
            vec!["VEIN IDENTIFIED 15 SCORE=66 TH=50 VIA=FV THS=50,60,70"]
        );
        let script = with_debug(vec![connected(), finger(Some(1)), verified(Some(15))], LOG_FP);
        assert_eq!(
            run_t(Command::Verify(15), script),
            vec!["VEIN VERIFIED 15 SCORE=0 TH=50 VIA=FP THS=50,60,70"]
        );
        // 4096 バイトまで複数の塊で読む
        let mut big = " ".repeat(DEBUG_LOG_MAX - LOG_FV.len());
        big.push_str(LOG_FV);
        let script = with_debug(vec![connected(), finger(Some(1)), verified(Some(15))], &big);
        assert_eq!(
            run_t(Command::Identify, script),
            vec!["VEIN IDENTIFIED 15 SCORE=66 TH=50 VIA=FV THS=50,60,70"]
        );
        // 要求は GET_DEBUG (データ無し) → READ_DATA 種別 0x26
        let mut port = FakePort::new(with_debug(vec![connected(), finger(Some(1)), verified(Some(15))], LOG_FV));
        super::run(Command::Identify, &mut None, &mut port, &Timeouts::DEFAULT, &mut |_| {});
        assert_eq!(port.sent[3], command(CMD_GET_DEBUG, &[]).to_vec());
        assert_eq!(port.sent[4], read_data_request(CMD_READ_DATA, CMD_GET_DEBUG, 0, LOG_FV.len() as u32).to_vec());
    }

    #[test]
    fn identify_falls_back_to_the_plain_line_when_the_log_fails() {
        let head = || vec![connected(), finger(Some(1)), verified(Some(15))];
        let plain = vec!["VEIN IDENTIFIED 15"];
        // 返事が無い
        assert_eq!(run_t(Command::Identify, head()), plain);
        // GET_DEBUG が失敗 (非対応) / 大きさが 0・上限超え
        let mut s = head();
        s.push(Some(reply(CMD_GET_DEBUG, &[XG_ERR_FAIL, XG_ERR_NO_SUPPORT])));
        assert_eq!(run_t(Command::Identify, s), plain);
        for n in [0u16, DEBUG_LOG_MAX as u16 + 1] {
            let mut s = head();
            s.push(Some(reply(CMD_GET_DEBUG, &[XG_ERR_SUCCESS, n as u8, (n >> 8) as u8])));
            assert_eq!(run_t(Command::Identify, s), plain);
        }
        // 記録が読めない形 / 塊が途中で途切れる
        assert_eq!(run_t(Command::Identify, with_debug(head(), "no match here")), plain);
        let mut s = head();
        s.push(Some(reply(CMD_GET_DEBUG, &[XG_ERR_SUCCESS, 0, 1])));
        s.push(Some(vec![0u8; 10]));
        assert_eq!(run_t(Command::Identify, s), plain);
        // 失敗したら入力を捨てて次の照合に残さない
        let mut port = FakePort::new(head());
        let discards = port.discards;
        let lines = super::run(Command::Identify, &mut None, &mut port, &Timeouts::DEFAULT, &mut |_| {});
        assert_eq!(lines, plain);
        assert_eq!(port.sent.len(), 4);
        assert!(port.discards > discards + 3);
        // 不一致は記録を読まない
        let mut port = FakePort::new(with_debug(vec![connected(), finger(Some(1)), verified(None)], LOG_FV));
        let lines = super::run(Command::Verify(15), &mut None, &mut port, &Timeouts::DEFAULT, &mut |_| {});
        assert_eq!(lines, vec!["VEIN NOMATCH"]);
        assert_eq!(port.sent.len(), 3);
    }

    #[test]
    fn debug_lines_split_clean_and_cut() {
        assert_eq!(debug_lines(""), vec!["VEIN TEST DEBUG END 0"]);
        assert_eq!(
            debug_lines("a b\r\n\n  \nUSER:1, th[1, 2, 3]\0\0\0"),
            vec!["VEIN TEST DEBUG a b", "VEIN TEST DEBUG USER:1, th[1, 2, 3]", "VEIN TEST DEBUG END 2"]
        );
        let long = "あ".repeat(DEBUG_LINE_MAX + 5);
        let l = &debug_lines(&long)[0];
        assert_eq!(l.chars().count(), "VEIN TEST DEBUG ".len() + DEBUG_LINE_MAX);
    }

    #[test]
    fn test_debug_connects_then_reads_the_log() {
        let mut script = vec![connected()];
        script.extend(debug_script(LOG_FP.as_bytes()));
        let mut port = FakePort::new(script);
        let lines = test_debug(&mut port, &Timeouts::DEFAULT).unwrap();
        assert_eq!(lines[0], "VEIN TEST DEBUG Match.User:15");
        assert_eq!(lines.last().unwrap(), "VEIN TEST DEBUG END 4");
        assert_eq!(port.sent[1], command(CMD_GET_DEBUG, &[]).to_vec());
        // 接続できない / 記録が読めない
        assert_eq!(test_debug(&mut FakePort::new(vec![None]), &Timeouts::DEFAULT), Err(VeinError::NoModule));
        let mut port = FakePort::new(vec![connected(), Some(reply(CMD_GET_DEBUG, &[XG_ERR_FAIL, XG_ERR_NO_SUPPORT]))]);
        assert_eq!(test_debug(&mut port, &Timeouts::DEFAULT), Err(VeinError::Rc(XG_ERR_NO_SUPPORT)));
    }

    #[test]
    fn verify_id_is_verify_with_the_id_after_settling() {
        let mut port = FakePort::new(vec![connected(), finger(Some(1)), verified(Some(42))]);
        assert_eq!(verify_id(&mut port, &Timeouts::DEFAULT, 42, &mut |_| {}), Ok(Some(42)));
        assert_eq!(port.sent[2], command(CMD_VERIFY, &[42, 0, 0, 0]).to_vec());
        assert_eq!(port.pauses, vec![IDENTIFY_SETTLE_MS]);
        assert_eq!(verify_line(42, Some(42), None), "VEIN VERIFIED 42");
        assert_eq!(verify_line(42, None, None), "VEIN NOMATCH");
        assert_eq!(sync_info_line(3, 100), "VEIN SYNC INFO USERS=3 MAX=100");
    }

    #[test]
    fn identify_errors() {
        let (r, _) = run_identify(vec![None]);
        assert_eq!(r, Err(VeinError::NoModule));
        let unsupported = Some(reply(CMD_VERIFY, &[XG_ERR_FAIL, XG_ERR_NO_SUPPORT]));
        assert_eq!(run_identify(vec![connected(), finger(Some(1)), unsupported]).0, Err(VeinError::Unsupported));
        let other = Some(reply(CMD_VERIFY, &[XG_ERR_FAIL, 0x07]));
        assert_eq!(run_identify(vec![connected(), finger(Some(1)), other]).0, Err(VeinError::Rc(0x07)));
        let unknown = Some(reply(CMD_VERIFY, &[0x05]));
        assert_eq!(run_identify(vec![connected(), finger(Some(1)), unknown]).0, Err(VeinError::Rc(0x05)));
        let timeout = Some(reply(CMD_VERIFY, &[XG_ERR_FAIL, XG_ERR_TIME_OUT]));
        assert_eq!(run_identify(vec![connected(), finger(Some(1)), timeout]).0, Err(VeinError::NoFinger));
    }

    #[test]
    fn record_lines_split_by_1024_bytes_and_end_with_the_total() {
        let rec: Vec<u8> = (0..1027u32).map(|i| i as u8).collect();
        let lines = record_lines(17, &rec);
        assert_eq!(lines.len(), 3);
        // "VEIN RECORD 17 0 " (17 文字) + 1024 バイト = 2048 文字
        assert_eq!(lines[0].len(), 17 + 2048);
        assert!(lines[0].starts_with("VEIN RECORD 17 0 000102030405"));
        assert_eq!(lines[1], "VEIN RECORD 17 1024 000102");
        assert_eq!(lines[2], "VEIN RECORD 17 END 1027");
        // 実機の大きさ (8156 バイト) は 8 行 + END。最長の行 (offset 6144) は 21 + 2048 = 2069 文字
        let lines = record_lines(100, &[0xAB; 8156]);
        assert_eq!(lines.len(), 9);
        assert_eq!(lines.iter().map(String::len).max(), Some(2069));
        assert!(lines[7].starts_with("VEIN RECORD 100 7168 ABAB"));
        assert_eq!(lines[8], "VEIN RECORD 100 END 8156");
    }

    /// `slot` を渡して 1 つ処理する。途中経過を受けるクロージャは 1 つにまとめる
    /// (SYNC は途中経過を出さないので、呼び出しごとに書くと呼ばれない関数が残る)
    fn run_in(slot: &mut Option<TemplateAssembler>, cmd: Command, script: Vec<Option<Vec<u8>>>) -> Vec<String> {
        super::run(cmd, slot, &mut FakePort::new(script), &Timeouts::DEFAULT, &mut |_| {})
    }

    fn run_t(cmd: Command, script: Vec<Option<Vec<u8>>>) -> Vec<String> {
        run_in(&mut None, cmd, script)
    }

    #[test]
    fn run_returns_the_lines_of_each_command() {
        let data = sample_chara(4);
        let script = vec![connected(), cleared(), enrolled(), size(4), Some(chunk(&data))];
        assert_eq!(
            run_t(Command::Enroll(19), script),
            vec!["VEIN RECORD 19 0 DEEDDEED".to_string(), "VEIN RECORD 19 END 4".to_string()]
        );
        assert_eq!(run_t(Command::Identify, vec![connected(), finger(Some(1)), verified(Some(3))]), vec!["VEIN IDENTIFIED 3"]);
        assert_eq!(run_t(Command::Identify, vec![connected(), finger(Some(1)), verified(None)]), vec!["VEIN NOMATCH"]);
        assert_eq!(
            run_t(Command::SyncWrite(chunk_cmd(5, 0, &TEMPLATE_MAGIC)), vec![]),
            vec!["VEIN SYNC WRITE 5 4"]
        );
        assert_eq!(run_t(Command::SyncClear(15), vec![connected(), cleared_ok()]), vec!["VEIN SYNC CLEARED 15"]);
        assert_eq!(
            run_t(Command::Verify(15), vec![connected(), finger(Some(1)), verified(Some(15))]),
            vec!["VEIN VERIFIED 15"]
        );
        assert_eq!(run_t(Command::Verify(15), vec![connected(), finger(Some(1)), verified(None)]), vec!["VEIN NOMATCH"]);
        let mut info = vec![XG_ERR_SUCCESS, 3, 0, 0, 0, 0, 0, 0, 0, 100, 0, 0, 0];
        info.resize(DATA_LEN, 0);
        assert_eq!(
            run_t(Command::SyncInfo, vec![connected(), Some(reply(CMD_GET_ENROLL_INFO, &info))]),
            vec!["VEIN SYNC INFO USERS=3 MAX=100"]
        );
        // 失敗は ERR の 1 行だけ (記録は出さない)
        assert_eq!(run_t(Command::SyncClear(15), vec![None]), vec!["ERR VEIN NO_MODULE"]);
        let script = vec![connected(), cleared(), Some(reply(CMD_ENROLL, &[XG_ERR_FAIL, XG_ERR_TIME_OUT]))];
        assert_eq!(run_t(Command::Enroll(19), script), vec!["ERR VEIN NO_FINGER"]);
        // run は渡された組み立て中のものを使う
        let mut slot = None;
        run_in(&mut slot, Command::SyncWrite(chunk_cmd(5, 0, &TEMPLATE_MAGIC)), vec![]);
        let lines = run_in(&mut slot, Command::SyncWrite(chunk_cmd(5, 4, &[1])), vec![]);
        assert_eq!(lines, vec!["VEIN SYNC WRITE 5 5"]);
    }

    #[test]
    fn module_id_range() {
        assert_eq!(parse_module_id("10"), Some(10));
        assert_eq!(parse_module_id("100"), Some(100));
        // 1 (作業用)・2〜9 (試験用)・範囲外・数字でないものは不可
        for bad in ["1", "2", "9", "0", "101", "", "-10", "+10", "10a", " 10", "99999999999"] {
            assert_eq!(parse_module_id(bad), None, "{bad:?}");
        }
    }

    /// 組み立て済みの slot (WRITE_LINE_MAX ずつ足した)
    fn assembled(id: u32, data: &[u8]) -> Option<TemplateAssembler> {
        let mut slot = None;
        for (i, part) in data.chunks(WRITE_LINE_MAX).enumerate() {
            write_t(&mut slot, &mut FakePort::new(vec![]), chunk_cmd(id, i * WRITE_LINE_MAX, part)).unwrap();
        }
        slot
    }

    #[test]
    fn sync_write_end_splits_by_512_and_resends() {
        let data = sample_chara(600);
        let mut slot = assembled(5, &data);
        // 1 塊目の応答が失敗 → 送り直して通る、2 塊目は 1 回で通る
        let mut port = FakePort::new(vec![connected(), write_ready(), ack_write(false), ack_write(true), ack_write(true)]);
        assert_eq!(write_t(&mut slot, &mut port, WriteCommand::End(5)).as_deref(), Ok("VEIN SYNC WRITTEN 5 600"));
        // 接続 → WRITE_ENROLL (ID 5、大きさ 600 = 0x258) → WRITE_DATA ×3
        assert_eq!(port.sent.len(), 5);
        assert_eq!(port.sent[1], command(CMD_WRITE_ENROLL, &[5, 0, 0, 0, 0x58, 0x02, 0, 0]).to_vec());
        let mut want = read_data_request(CMD_WRITE_DATA, CMD_WRITE_ENROLL, 0, 512).to_vec();
        want.extend_from_slice(&data[..512]);
        want.extend_from_slice(&checksum(&data[..512]).to_le_bytes());
        assert_eq!(port.sent[2], want);
        assert_eq!(port.sent[3], want);
        let mut want = read_data_request(CMD_WRITE_DATA, CMD_WRITE_ENROLL, 512, 88).to_vec();
        want.extend_from_slice(&data[512..]);
        want.extend_from_slice(&checksum(&data[512..]).to_le_bytes());
        assert_eq!(port.sent[4], want);
    }

    #[test]
    fn sync_write_end_gives_up_after_three_tries() {
        // 無応答・別コマンドの応答・失敗が続くと諦める
        let mut slot = assembled(5, &TEMPLATE_MAGIC);
        let other = Some(reply(CMD_READ_DATA, &[XG_ERR_SUCCESS]));
        let mut port = FakePort::new(vec![connected(), write_ready(), None, other, ack_write(false)]);
        assert_eq!(write_t(&mut slot, &mut port, WriteCommand::End(5)), Err(VeinError::ReadFail));
        assert_eq!(port.sent.len(), 5);
        // 送信に失敗しても 3 回まで繰り返す (WRITE_DATA は 3 回目以降の送信)
        let mut slot = assembled(5, &TEMPLATE_MAGIC);
        let mut port = FailAt(FakePort::new(vec![connected(), write_ready()]), 3);
        assert_eq!(write_t(&mut slot, &mut port, WriteCommand::End(5)), Err(VeinError::ReadFail));
        assert_eq!(port.0.sent.len(), 5);
    }

    /// 抜き出した後も capture は今までと同じバイトを同じ順に送る (ID 1)
    #[test]
    fn capture_still_sends_scratch_id_bytes() {
        let data = vec![0xDE, 0xED, 1, 2];
        let mut script = head(4);
        script.push(Some(chunk(&data)));
        script.push(cleared());
        let mut port = FakePort::new(script);
        assert_eq!(run(&mut port), Ok(data));
        assert_eq!(
            port.sent[1],
            command(CMD_CLEAR_ENROLL, &[1, 0, 0, 0]).to_vec()
        );
        assert_eq!(
            port.sent[2],
            command(CMD_ENROLL, &[1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 2, 0]).to_vec()
        );
    }
}
