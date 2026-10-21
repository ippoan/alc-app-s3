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
//!   1 回だけ登録 → READ_ENROLL (0x22) で大きさ → READ_DATA の種別 0x22 で読む」で取る。
//!   **モジュール内の登録領域はこの作業用 ID にしか使わない** (照合はホストが持つ)
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
/// 試験用 (`VEIN TEST COPY|WRITE`) だけが使う
pub const CMD_WRITE_DATA: u8 = 0x21;
/// 指定 ID の登録データを書く前の確認 (WRITE_ENROLL、§2.2.27。ID と大きさを渡す)
pub const CMD_WRITE_ENROLL: u8 = 0x23;
/// 指定 ID の登録テンプレート数 (GET_ID_INFO、§2.2.19。0 件 = 空き ID)
pub const CMD_GET_ID_INFO: u8 = 0x15;

/// 指定 ID の指で照合する (1:1)。**試験用 (`VEIN TEST VERIFY`)。番号は `0x17` (出典は
/// vein-match の調査メモ)、引数の並び (`bData[0..4]` = ID LE) と応答の形は推測 —
/// WS-FVS10 で通るかは実機でしか分からない**。非対応は `01 10`
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
/// 撮像に失敗したときにモジュールが撮り直す回数 (`bData[10]`)。1 回の待ちは最大 5 秒なので、
/// ホスト (alc-app `useVeinSerial` の `CAPTURE_TIMEOUT_MS` = 20 秒) に収まる回数にする
pub const ENROLL_RETRIES: u8 = 2;

/// 試験用コマンド (`VEIN TEST …`) が使える ID の範囲。**1 は読み取りの作業用 ([`SCRATCH_ID`])
/// で毎回消されるので使わない**
pub const TEST_ID_MIN: u32 = 2;
pub const TEST_ID_MAX: u32 = 9;
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

/// `VEIN TEST WRITE` 1 行で受ける登録データの最大バイト数 (hex で 384 文字。
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
    /// コマンド非対応 (`01 10`)。試験用 VERIFY で、この機種では通らないと分かった
    Unsupported,
    /// 試験用 WRITE: offset が続いていない・組み立て中のものが無い・ID が違う
    WriteSeq,
    /// 試験用 WRITE: 合計が [`CHARA_MAX`] を超える
    WriteTooBig,
    /// 試験用 WRITE: 先頭が [`TEMPLATE_MAGIC`] でない
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
/// 読み取り ([`capture`]) と試験 ([`test_enroll`]) が共有する。送るバイトは ID だけが違う。
/// 途中経過 (置いて / 離して / 撮り直し) は `on_prompt` へ渡す
fn enroll_id(
    port: &mut dyn Port,
    t: &Timeouts,
    id: u32,
    on_prompt: &mut dyn FnMut(Prompt),
) -> Result<(), VeinError> {
    // 前回が途中で終わった残りを消す。未登録なら失敗が返るが、応答さえあればよい
    let id = id.to_le_bytes();
    exchange(port, t, CMD_CLEAR_ENROLL, &id)?;

    let mut req = [0u8; 12];
    req[..4].copy_from_slice(&id);
    req[5] = ENROLL_TEMPLATES;
    req[10] = ENROLL_RETRIES;
    let p = send_and_await_final(port, t, CMD_ENROLL, &req, on_prompt)?;
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
/// 指が置かれず時間切れ (`01 0B`) は [`VeinError::NoFinger`]、撮り直しが
/// [`ENROLL_RETRIES`] 回を超えたら [`VeinError::Rc`]、途中経過が [`MAX_PROMPTS`] を超えて
/// 続いたら [`VeinError::ReadFail`]
fn send_and_await_final(
    port: &mut dyn Port,
    t: &Timeouts,
    cmd: u8,
    req: &[u8],
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
                // 撮り直しはモジュールが ENROLL_RETRIES 回まで続ける。それを超えて届いたら終わり
                retries += 1;
                if retries > ENROLL_RETRIES {
                    return Err(VeinError::Rc(code));
                }
                on_prompt(Prompt::Retry(code));
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
    enroll_id(port, t, SCRATCH_ID, on_prompt)?;
    // 4.-5. 登録データを読む
    let out = read_enroll(port, t, SCRATCH_ID)?;

    // 6. 作業用 ID を消して終わる。失敗しても読めた分は返す (次の読み取りの 2. で消える)
    let _ = exchange(port, t, CMD_CLEAR_ENROLL, &SCRATCH_ID.to_le_bytes());
    Ok(out)
}

/// ID `id` の登録データを読む (接続済みであること。READ_ENROLL → READ_DATA)。
/// 読み取り ([`capture`]) と試験 ([`test_copy`]) が共有する
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

    // 分割読み出し (種別 = READ_ENROLL)。塊ごとに和を確かめ、合わなければ読み直す
    let mut out = Vec::with_capacity(size);
    let mut buf = vec![0u8; UART_DATA_PACKET_MAX + 2];
    for (offset, len) in chunks(size) {
        let mut ok = false;
        for _ in 0..READ_RETRIES {
            port.discard_input();
            let req = read_data_request(CMD_READ_DATA, CMD_READ_ENROLL, offset as u32, len as u32);
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
/// 運用の画面からは送らない
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestCommand {
    /// `VEIN TEST ENROLL <id>`: ID に登録してモジュールに残す
    Enroll(u32),
    /// `VEIN TEST VERIFY <id>`: ID で 1:1 照合する
    Verify(u32),
    /// `VEIN TEST CLEAR <id>`: ID の登録を消す
    Clear(u32),
    /// `VEIN TEST INFO <id>`: ID の登録テンプレート数と READ_ENROLL の大きさだけを返す
    Info(u32),
    /// `VEIN TEST COPY <from> <to>`: `from` の登録データを読み、`to` へ書く
    Duplicate { from: u32, to: u32 },
    /// `VEIN TEST REBUILD <a> [<b>] <to> [U]`: `a` (と `b`) の登録データから特徴量を取り出し、
    /// サーバーと同じ作り直し (`create_template`) で記録を組んで `to` へ書く。`b` があれば
    /// 件の数 2、無ければ 1。`user` は利用者番号を `a` の記録の値にする (無ければ 0)
    Rebuild { a: u32, b: Option<u32>, to: u32, user: bool },
}

/// `VEIN TEST WRITE …` (試験用)。外から受けた登録データを ID へ書く。
/// **データ (生体情報) を持つので、`Debug` は大きさだけを出す** (ログに hex を残さない)
#[derive(Clone, PartialEq, Eq)]
pub enum WriteCommand {
    /// `VEIN TEST WRITE <id> <offset> <hex>`: 組み立て中のデータへ続ける (offset 0 は最初から)
    Chunk { id: u32, offset: usize, data: Vec<u8> },
    /// `VEIN TEST WRITE <id> END`: 組み上がったものを ID へ書く
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

/// 試験用の ID を読む。[`TEST_ID_MIN`]〜[`TEST_ID_MAX`] の数字だけ (1 は作業用なので不可)
pub fn parse_test_id(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<u32>()
        .ok()
        .filter(|id| (TEST_ID_MIN..=TEST_ID_MAX).contains(id))
}

/// 試験: 接続して ID に登録する。モジュールに残す (READ_ENROLL も削除もしない)
pub fn test_enroll(
    port: &mut dyn Port,
    t: &Timeouts,
    id: u32,
    on_prompt: &mut dyn FnMut(Prompt),
) -> Result<(), VeinError> {
    connect(port, t)?;
    enroll_id(port, t, id, on_prompt)
}

/// 試験: 接続して ID で 1:1 照合する。戻り値は最終応答のデータ部 (`bData[0]` = RC)。
/// **引数は ENROLL と同じ形 (`bData[0..4]` = ID LE) の推測**。待ち方も推測で、
/// ENROLL と同じく VERIFY 送信後にモジュールが指を待ち、途中経過 (置いて / 離して) が届く前提
/// (指を待たず即座に「指が無い」を返す機種なら FINGER_STATE で待ってから送る形に直す — 実機で確認)。
/// 非対応 (`01 10`) は [`VeinError::Unsupported`]
pub fn test_verify(
    port: &mut dyn Port,
    t: &Timeouts,
    id: u32,
    on_prompt: &mut dyn FnMut(Prompt),
) -> Result<[u8; DATA_LEN], VeinError> {
    connect(port, t)?;
    let p = send_and_await_final(port, t, CMD_VERIFY, &id.to_le_bytes(), on_prompt)?;
    if p.data[0] == XG_ERR_FAIL && p.data[1] == XG_ERR_NO_SUPPORT {
        return Err(VeinError::Unsupported);
    }
    Ok(p.data)
}

/// 試験: 接続して ID の登録を消す。未登録の ID は失敗コード (`ERR VEIN RC=…`) が返る
pub fn test_clear(port: &mut dyn Port, t: &Timeouts, id: u32) -> Result<(), VeinError> {
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

/// 試験: 接続して ID のテンプレート数 (GET_ID_INFO) と READ_ENROLL が返す大きさを読む。
/// 登録データそのものは読まない。大きさは `bData[1..5]` LE (§2.2.26)。空の ID は 0
pub fn test_info(port: &mut dyn Port, t: &Timeouts, id: u32) -> Result<(u8, u32), VeinError> {
    connect(port, t)?;
    let p = exchange(port, t, CMD_GET_ID_INFO, &id.to_le_bytes())?;
    if p.data[0] != XG_ERR_SUCCESS {
        return Err(VeinError::Rc(p.data[1]));
    }
    let count = p.data[1];
    let p = exchange(port, t, CMD_READ_ENROLL, &id.to_le_bytes())?;
    let size = if p.data[0] == XG_ERR_SUCCESS {
        u32::from_le_bytes([p.data[1], p.data[2], p.data[3], p.data[4]])
    } else {
        0
    };
    Ok((count, size))
}

/// `VEIN TEST INFO <id> TEMPLATES=<n> SIZE=<バイト数>`
pub fn test_info_line(id: u32, count: u8, size: u32) -> String {
    format!("VEIN TEST INFO {id} TEMPLATES={count} SIZE={size}")
}

/// ID `id` の登録データを書く (接続済みであること。WRITE_ENROLL → WRITE_DATA、§2.2.27 / §2.2.25)。
/// `data` は 1 バイト以上 [`CHARA_MAX`] 以下。塊ごとに和を付けて送り、応答が成功でなければ
/// 3 回まで送り直す。**フラッシュに書く**ので試験用の ID (2〜9) だけに使う
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

/// 試験: 接続して `from` の登録データを読み、`to` へ書く。戻り値は書いたバイト数
pub fn test_copy(port: &mut dyn Port, t: &Timeouts, from: u32, to: u32) -> Result<usize, VeinError> {
    connect(port, t)?;
    let data = read_enroll(port, t, from)?;
    write_enroll(port, t, to, &data)?;
    Ok(data.len())
}

/// `VEIN TEST COPIED <from> <to> <バイト数>`
pub fn test_copied_line(from: u32, to: u32, len: usize) -> String {
    format!("VEIN TEST COPIED {from} {to} {len}")
}

/// 試験: `a` (と `b`) の登録データを読み、特徴量だけ取り出して `create_template` で記録を
/// 作り直し (サーバーの画面の登録と同じ: group 0, mode 1, 詰め物なし)、`to` へ書く。
/// 戻り値は応答の 1 行 (`VEIN TEST REBUILT …`)。
///
/// 行に出すのは `a` の外側ヘッダの値 (利用者番号 +0x10 / 件の数 +0x15 / グループ +0x16。暗号化
/// されていない数値で生体データではない) と、件の数 1 のときの `a` との違うバイト数だけ。
/// 位置も中身も出さない。`create_template` の失敗の番号は応答の破損 ([`VeinError::ReadFail`])
/// にまとめる
pub fn test_rebuild(
    port: &mut dyn Port,
    t: &Timeouts,
    a: u32,
    b: Option<u32>,
    to: u32,
    user: bool,
) -> Result<String, VeinError> {
    connect(port, t)?;
    let src = read_enroll(port, t, a)?;
    let mut charas = vec![extract_chara(&src)?];
    if let Some(b) = b {
        charas.push(extract_chara(&read_enroll(port, t, b)?)?);
    }
    // extract_chara が通った = 外側ヘッダ (0x20 バイト) を読めている
    let src_user = u32::from_le_bytes([src[0x10], src[0x11], src[0x12], src[0x13]]);
    let refs: Vec<&[u8]> = charas.iter().map(|c| c.as_slice()).collect();
    let rebuilt = vein_match_search::create_template(&refs, if user { src_user } else { 0 }, 0, 1, &[])
        .map_err(|_| VeinError::ReadFail)?;
    write_enroll(port, t, to, &rebuilt)?;
    let diff = if b.is_none() && rebuilt.len() == src.len() {
        src.iter().zip(&rebuilt).filter(|(x, y)| x != y).count().to_string()
    } else {
        "-".into()
    };
    Ok(format!(
        "VEIN TEST REBUILT {to} {} SRC_USER={src_user} SRC_GROUP={} SRC_COUNT={} DIFF={diff} FLAGS={}",
        rebuilt.len(),
        src[0x16],
        src[0x15],
        if user { "U" } else { "" },
    ))
}

/// 試験: 外から受けた登録データを足す / 書く。`slot` は組み立て中のもの (スレッドが持つ)。
/// offset 0 は組み立てを最初からやり直す。失敗したら組み立て中のものは捨てる。
/// 戻り値は応答の 1 行: 塊なら `VEIN TEST WRITE <id> <次の offset>`、END なら
/// `VEIN TEST WRITTEN <id> <バイト数>`。**data を行にもログにも出さない**
pub fn test_write(
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
            Ok(format!("VEIN TEST WRITE {id} {next}"))
        }
        WriteCommand::End(id) => {
            let asm = slot.take().filter(|a| a.id == id).ok_or(VeinError::WriteSeq)?;
            if !asm.buf.starts_with(&TEMPLATE_MAGIC) {
                return Err(VeinError::WriteHeader);
            }
            connect(port, t)?;
            write_enroll(port, t, id, &asm.buf)?;
            Ok(format!("VEIN TEST WRITTEN {id} {}", asm.buf.len()))
        }
    }
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
    fn test_clear_sends_clear_for_the_id() {
        let mut port = FakePort::new(vec![connected(), Some(reply(CMD_CLEAR_ENROLL, &[XG_ERR_SUCCESS]))]);
        assert_eq!(test_clear(&mut port, &Timeouts::DEFAULT, 4), Ok(()));
        assert_eq!(&port.sent[1][3..10], &[CMD_CLEAR_ENROLL, 0, 4, 4, 0, 0, 0]);
        // 未登録などの失敗コードは Rc で返す
        let mut port = FakePort::new(vec![connected(), cleared()]);
        assert_eq!(test_clear(&mut port, &Timeouts::DEFAULT, 4), Err(VeinError::Rc(0x07)));
        let mut port = FakePort::new(vec![connected(), None]);
        assert_eq!(test_clear(&mut port, &Timeouts::DEFAULT, 4), Err(VeinError::Timeout));
        let mut port = FakePort::new(vec![None]);
        assert_eq!(test_clear(&mut port, &Timeouts::DEFAULT, 4), Err(VeinError::NoModule));
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
    fn test_info_reads_template_count_and_size() {
        let mut port = FakePort::new(vec![
            connected(),
            Some(reply(CMD_GET_ID_INFO, &[XG_ERR_SUCCESS, 2])),
            Some(reply(CMD_READ_ENROLL, &[XG_ERR_SUCCESS, 0x48, 0x04, 0, 0])),
        ]);
        assert_eq!(test_info(&mut port, &Timeouts::DEFAULT, 4), Ok((2, 0x448)));
        assert_eq!(port.sent.len(), 3);
        assert_eq!(&port.sent[1][3..10], &[CMD_GET_ID_INFO, 0, 4, 4, 0, 0, 0]);
        assert_eq!(&port.sent[2][3..10], &[CMD_READ_ENROLL, 0, 4, 4, 0, 0, 0]);
        // 空の ID: READ_ENROLL が失敗でも大きさ 0 で返す
        let mut port = FakePort::new(vec![
            connected(),
            Some(reply(CMD_GET_ID_INFO, &[XG_ERR_SUCCESS, 0])),
            Some(reply(CMD_READ_ENROLL, &[XG_ERR_FAIL, 0x0A])),
        ]);
        assert_eq!(test_info(&mut port, &Timeouts::DEFAULT, 4), Ok((0, 0)));
        // GET_ID_INFO の失敗コードは Rc、応答が無ければ Timeout
        let mut port = FakePort::new(vec![connected(), Some(reply(CMD_GET_ID_INFO, &[XG_ERR_FAIL, 0x07]))]);
        assert_eq!(test_info(&mut port, &Timeouts::DEFAULT, 4), Err(VeinError::Rc(0x07)));
        let mut port = FakePort::new(vec![connected(), None]);
        assert_eq!(test_info(&mut port, &Timeouts::DEFAULT, 4), Err(VeinError::Timeout));
        assert_eq!(test_info_line(4, 2, 1096), "VEIN TEST INFO 4 TEMPLATES=2 SIZE=1096");
    }

    #[test]
    fn test_copy_reads_from_and_writes_to() {
        let data = sample_chara(600);
        let mut script = vec![connected(), size(600)];
        for (off, len) in chunks(600) {
            script.push(Some(chunk(&data[off..off + len])));
        }
        script.extend([write_ready(), ack_write(true), ack_write(true)]);
        let mut port = FakePort::new(script);
        assert_eq!(test_copy(&mut port, &Timeouts::DEFAULT, 2, 3), Ok(600));
        // 接続 → READ_ENROLL → READ_DATA ×2 → WRITE_ENROLL → WRITE_DATA ×2
        assert_eq!(port.sent.len(), 7);
        assert_eq!(&port.sent[1][3..10], &[CMD_READ_ENROLL, 0, 4, 2, 0, 0, 0]);
        // 書き込み先 ID 3 と大きさ 600 = 0x258
        assert_eq!(
            port.sent[4],
            command(CMD_WRITE_ENROLL, &[3, 0, 0, 0, 0x58, 0x02, 0, 0]).to_vec()
        );
        // WRITE_DATA: 要求 24 バイト + 本体 + 和 (本体だけの和、LE)
        let mut want = read_data_request(CMD_WRITE_DATA, CMD_WRITE_ENROLL, 0, 512).to_vec();
        want.extend_from_slice(&data[..512]);
        want.extend_from_slice(&checksum(&data[..512]).to_le_bytes());
        assert_eq!(port.sent[5], want);
        let mut want = read_data_request(CMD_WRITE_DATA, CMD_WRITE_ENROLL, 512, 88).to_vec();
        want.extend_from_slice(&data[512..]);
        want.extend_from_slice(&checksum(&data[512..]).to_le_bytes());
        assert_eq!(port.sent[6], want);
        assert_eq!(test_copied_line(2, 3, 600), "VEIN TEST COPIED 2 3 600");
    }

    #[test]
    fn test_copy_failures() {
        // 読めない (空の ID) / 接続できない
        let mut port = FakePort::new(vec![connected(), Some(reply(CMD_READ_ENROLL, &[XG_ERR_FAIL, 0x0A]))]);
        assert_eq!(test_copy(&mut port, &Timeouts::DEFAULT, 2, 3), Err(VeinError::Rc(0x0A)));
        let mut port = FakePort::new(vec![None]);
        assert_eq!(test_copy(&mut port, &Timeouts::DEFAULT, 2, 3), Err(VeinError::NoModule));
        // WRITE_ENROLL が断られた
        let data = sample_chara(4);
        let head = vec![connected(), size(4), Some(chunk(&data))];
        let mut script = head.clone();
        script.push(Some(reply(CMD_WRITE_ENROLL, &[XG_ERR_FAIL, 0x03])));
        let mut port = FakePort::new(script);
        assert_eq!(test_copy(&mut port, &Timeouts::DEFAULT, 2, 3), Err(VeinError::Rc(0x03)));
        // WRITE_DATA の応答が失敗 → 送り直し、2 回目で通る
        let mut script = head.clone();
        script.extend([write_ready(), ack_write(false), ack_write(true)]);
        let mut port = FakePort::new(script);
        assert_eq!(test_copy(&mut port, &Timeouts::DEFAULT, 2, 3), Ok(4));
        assert_eq!(port.sent.len(), 6);
        // 無応答・別コマンドの応答が 3 回続くと諦める
        let mut script = head.clone();
        script.extend([write_ready(), None, Some(reply(CMD_READ_DATA, &[XG_ERR_SUCCESS])), ack_write(false)]);
        let mut port = FakePort::new(script);
        assert_eq!(test_copy(&mut port, &Timeouts::DEFAULT, 2, 3), Err(VeinError::ReadFail));
        assert_eq!(port.sent.len(), 7);
        // 送信に失敗しても 3 回まで繰り返す (WRITE_DATA は 5 回目以降の送信)
        let mut script = head;
        script.push(write_ready());
        let mut port = FailAt(FakePort::new(script), 5);
        assert_eq!(test_copy(&mut port, &Timeouts::DEFAULT, 2, 3), Err(VeinError::ReadFail));
    }

    /// REBUILD の台本: 接続 → (READ_ENROLL → READ_DATA …) を元の数だけ → WRITE_ENROLL → WRITE_DATA …
    fn rebuild_script(srcs: &[&[u8]], out_len: usize) -> Vec<Option<Vec<u8>>> {
        let mut script = vec![connected()];
        for src in srcs {
            script.push(size(src.len()));
            for (off, len) in chunks(src.len()) {
                script.push(Some(chunk(&src[off..off + len])));
            }
        }
        script.push(write_ready());
        script.extend(chunks(out_len).map(|_| ack_write(true)));
        script
    }

    fn rebuild_t(port: &mut dyn Port, b: Option<u32>, user: bool) -> Result<String, VeinError> {
        test_rebuild(port, &Timeouts::DEFAULT, 2, b, 3, user)
    }

    #[test]
    fn test_rebuild_one_chara_matches_the_server_shape() {
        // サーバーと同じ作り直し (user 0) で組んだ記録を元にすれば、作り直しても同じ
        let src = synth_enroll(&[&synth_chara(1)]);
        let mut port = FakePort::new(rebuild_script(&[&src], src.len()));
        let line = rebuild_t(&mut port, None, false).unwrap();
        assert_eq!(
            line,
            format!("VEIN TEST REBUILT 3 {} SRC_USER=0 SRC_GROUP=0 SRC_COUNT=1 DIFF=0 FLAGS=", src.len())
        );
        // 接続 → READ_ENROLL → READ_DATA ×n → WRITE_ENROLL → WRITE_DATA ×m
        let n = chunks(src.len()).count();
        assert_eq!(port.sent.len(), 3 + 2 * n);
        assert_eq!(&port.sent[1][3..10], &[CMD_READ_ENROLL, 0, 4, 2, 0, 0, 0]);
        let len = (src.len() as u32).to_le_bytes();
        assert_eq!(
            port.sent[2 + n],
            command(CMD_WRITE_ENROLL, &[3, 0, 0, 0, len[0], len[1], len[2], len[3]]).to_vec()
        );
        let mut want = read_data_request(CMD_WRITE_DATA, CMD_WRITE_ENROLL, 0, 512).to_vec();
        want.extend_from_slice(&src[..512]);
        want.extend_from_slice(&checksum(&src[..512]).to_le_bytes());
        assert_eq!(port.sent[3 + n], want);
    }

    #[test]
    fn test_rebuild_user_flag_takes_the_source_user() {
        // 利用者番号 5 の記録: flag なしは 0 に作り直すので違いが出る、U は 5 を引き継ぐ
        let src = vein_match_search::create_template(&[&synth_chara(1)[..]], 5, 0, 1, &[]).unwrap();
        let mut port = FakePort::new(rebuild_script(&[&src], src.len()));
        let line = rebuild_t(&mut port, None, false).unwrap();
        assert!(line.contains(" SRC_USER=5 SRC_GROUP=0 SRC_COUNT=1 DIFF="), "{line}");
        assert!(!line.contains("DIFF=0 "), "{line}");
        let mut port = FakePort::new(rebuild_script(&[&src], src.len()));
        let line = rebuild_t(&mut port, None, true).unwrap();
        assert_eq!(
            line,
            format!("VEIN TEST REBUILT 3 {} SRC_USER=5 SRC_GROUP=0 SRC_COUNT=1 DIFF=0 FLAGS=U", src.len())
        );
    }

    #[test]
    fn test_rebuild_two_charas_makes_a_two_entry_record() {
        let a = synth_enroll(&[&synth_chara(1)]);
        let b = synth_enroll(&[&synth_chara(2)]);
        let want = synth_enroll(&[&synth_chara(1), &synth_chara(2)]);
        let mut port = FakePort::new(rebuild_script(&[&a, &b], want.len()));
        let line = rebuild_t(&mut port, Some(4), false).unwrap();
        // 件の数 2 は元の記録と長さが違うので DIFF は数えない。SRC_* は a の値
        assert_eq!(
            line,
            format!("VEIN TEST REBUILT 3 {} SRC_USER=0 SRC_GROUP=0 SRC_COUNT=1 DIFF=- FLAGS=", want.len())
        );
        // 2 つ目は ID 4 から読む
        let n = chunks(a.len()).count();
        assert_eq!(&port.sent[2 + n][3..10], &[CMD_READ_ENROLL, 0, 4, 4, 0, 0, 0]);
        // 元が 2 件の記録は特徴量を 1 つに決められないので断る
        let mut port = FakePort::new(rebuild_script(&[&want], a.len()));
        assert_eq!(rebuild_t(&mut port, None, false), Err(VeinError::ReadFail));
    }

    #[test]
    fn test_rebuild_failures() {
        let src = synth_enroll(&[&synth_chara(1)]);
        // 読めない (空の ID) / 接続できない
        let mut port = FakePort::new(vec![connected(), Some(reply(CMD_READ_ENROLL, &[XG_ERR_FAIL, 0x0A]))]);
        assert_eq!(rebuild_t(&mut port, None, false), Err(VeinError::Rc(0x0A)));
        let mut port = FakePort::new(vec![None]);
        assert_eq!(rebuild_t(&mut port, None, false), Err(VeinError::NoModule));
        // 読めたが登録データの形ではない
        let junk = sample_chara(0x40);
        let mut port = FakePort::new(rebuild_script(&[&junk], 0));
        assert_eq!(rebuild_t(&mut port, None, false), Err(VeinError::ReadFail));
        // 2 つ目が読めない / 形ではない
        let mut script = rebuild_script(&[&src], 0);
        script.truncate(script.len() - 1);
        script.push(Some(reply(CMD_READ_ENROLL, &[XG_ERR_FAIL, 0x0A])));
        let mut port = FakePort::new(script);
        assert_eq!(rebuild_t(&mut port, Some(4), false), Err(VeinError::Rc(0x0A)));
        let mut port = FakePort::new(rebuild_script(&[&src, &junk], 0));
        assert_eq!(rebuild_t(&mut port, Some(4), false), Err(VeinError::ReadFail));
        // 特徴量が壊れていて作り直せない (件の先頭の印を潰した記録)
        let mut rec = vein_match_search::make_record(&[[0u8; vein_match::PLANE_LEN]], 0, 0).unwrap();
        rec[0x40] = 0;
        let bad = vein_match_search::build_template(&rec, 0, 1, &[]).unwrap();
        let mut port = FakePort::new(rebuild_script(&[&bad], 0));
        assert_eq!(rebuild_t(&mut port, None, false), Err(VeinError::ReadFail));
        // 書き込み先が断った
        let mut script = rebuild_script(&[&src], 0);
        script.push(Some(reply(CMD_WRITE_ENROLL, &[XG_ERR_FAIL, 0x03])));
        let mut port = FakePort::new(script);
        assert_eq!(rebuild_t(&mut port, None, false), Err(VeinError::Rc(0x03)));
    }

    fn chunk_cmd(id: u32, offset: usize, data: &[u8]) -> WriteCommand {
        WriteCommand::Chunk { id, offset, data: data.to_vec() }
    }

    fn write_t(slot: &mut Option<TemplateAssembler>, port: &mut dyn Port, cmd: WriteCommand) -> Result<String, VeinError> {
        test_write(slot, port, &Timeouts::DEFAULT, cmd)
    }

    #[test]
    fn test_write_assembles_lines_then_writes_on_end() {
        let data = sample_chara(300);
        let mut slot = None;
        let mut port = FakePort::new(vec![connected(), write_ready(), ack_write(true)]);
        assert_eq!(
            write_t(&mut slot, &mut port, chunk_cmd(5, 0, &data[..192])).as_deref(),
            Ok("VEIN TEST WRITE 5 192")
        );
        assert_eq!(
            write_t(&mut slot, &mut port, chunk_cmd(5, 192, &data[192..])).as_deref(),
            Ok("VEIN TEST WRITE 5 300")
        );
        // 塊の間はモジュールに触らない
        assert!(port.sent.is_empty());
        assert_eq!(
            write_t(&mut slot, &mut port, WriteCommand::End(5)).as_deref(),
            Ok("VEIN TEST WRITTEN 5 300")
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
    fn test_write_sequence_errors_discard_the_buffer() {
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
            Ok("VEIN TEST WRITE 6 2")
        );
        assert_eq!(slot.as_ref().map(|a| a.id), Some(6));
        assert!(port.sent.is_empty());
    }

    #[test]
    fn test_write_limits_size_and_checks_header() {
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
            Ok(format!("VEIN TEST WRITE 5 {CHARA_MAX}").as_str())
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
    fn test_write_end_reports_module_errors() {
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
