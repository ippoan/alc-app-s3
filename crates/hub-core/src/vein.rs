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
//! - 中身は**ここでは解釈しない** — モジュールが返した大きさのまま 16 進で出す
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

/// 特徴量を取り出すために 1 回だけ登録する作業用の ID。読み取りの前後で消す
pub const SCRATCH_ID: u32 = 1;
/// 1 回の登録で撮るテンプレートの数 (`bData[5]`)。読み取り 1 回 = 撮像 1 回
pub const ENROLL_TEMPLATES: u8 = 1;
/// 撮像に失敗したときにモジュールが撮り直す回数 (`bData[10]`)。1 回の待ちは最大 5 秒なので、
/// ホスト (alc-app `useVeinSerial` の `CAPTURE_TIMEOUT_MS` = 20 秒) に収まる回数にする
pub const ENROLL_RETRIES: u8 = 2;

/// 工場出荷時の接続パスワード ("0" × 8、§2.2.1)
pub const DEFAULT_PASSWORD: &[u8; 8] = b"00000000";

/// 成功
pub const XG_ERR_SUCCESS: u8 = 0x00;
/// 失敗 (`bData[1]` が理由のエラーコード)
pub const XG_ERR_FAIL: u8 = 0x01;
/// 指の入力待ちがモジュール側で時間切れ
pub const XG_ERR_TIME_OUT: u8 = 0x0B;
/// 「指を置いて」の途中経過 (登録中に届く)
pub const XG_INPUT_FINGER: u8 = 0x20;
/// 「指を離して」の途中経過 (登録中に届く)
pub const XG_RELEASE_FINGER: u8 = 0x21;

/// READ_DATA の 1 回の上限 (**UART は 512 バイト**。USB は 4096、§1.2.6)
pub const UART_DATA_PACKET_MAX: usize = 512;
/// 特徴量 (登録データ) として受け取る大きさの上限。実機の登録データは 0x1FDC (8156) バイト
/// (2026-10-07) なので 8KB とする — これを超える値は応答の破損とみなす
pub const CHARA_MAX: usize = 0x2000;

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

/// READ_DATA の要求 (§1.2.6): `bData[0]` = 種別、`[1..4]` = オフセット、
/// `[5..8]` = 大きさ (どちらも LE)
pub fn read_data_request(kind: u8, offset: u32, size: u32) -> [u8; PACKET_LEN] {
    let mut d = [0u8; 9];
    d[0] = kind;
    d[1..5].copy_from_slice(&offset.to_le_bytes());
    d[5..9].copy_from_slice(&size.to_le_bytes());
    command(CMD_READ_DATA, &d)
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

/// 接続して作業用 ID に 1 回登録し、その登録データを読み出す
/// (§2.2.1 → §2.2.15 → §2.2.20 → §2.2.26 → §1.2.6 → §2.2.15)。
///
/// 戻り値はモジュールが返した大きさそのままのバイト列 (中身は解釈しない)。
/// 登録中の途中経過は `on_prompt` へ渡す (端末が「離して」の合図を鳴らす)。
/// 案内音声 (置いて / もう一度) はホストが `VEIN SAY` で鳴らす
pub fn capture(
    port: &mut dyn Port,
    t: &Timeouts,
    on_prompt: &mut dyn FnMut(Prompt),
) -> Result<Vec<u8>, VeinError> {
    // 1. 接続。応答が無ければモジュールが居ない
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

    // 2. 作業用 ID を空ける (前回の読み取りが途中で終わった残り)。未登録なら失敗が
    //    返るが、応答さえあればよい
    let id = SCRATCH_ID.to_le_bytes();
    exchange(port, t, CMD_CLEAR_ENROLL, &id)?;

    // 3. 1 回だけ登録する。途中経過 (置いて / 離して) を渡しながら完了を待つ
    let mut req = [0u8; 12];
    req[..4].copy_from_slice(&id);
    req[5] = ENROLL_TEMPLATES;
    req[10] = ENROLL_RETRIES;
    port.discard_input();
    if !port.send(&command(CMD_ENROLL, &req)) {
        return Err(VeinError::ReadFail);
    }
    let mut enrolled = false;
    for _ in 0..=MAX_PROMPTS {
        let p = recv_packet(port, t.finger_ms, t.idle_ms).map_err(|e| match e {
            RecvError::Silent => VeinError::Timeout,
            RecvError::Broken => VeinError::ReadFail,
        })?;
        if p.cmd != CMD_ENROLL {
            return Err(VeinError::ReadFail);
        }
        match enroll_reply(&p) {
            EnrollReply::Done => {
                enrolled = true;
                break;
            }
            EnrollReply::Prompt(x) => on_prompt(x),
            EnrollReply::Failed(XG_ERR_TIME_OUT) => return Err(VeinError::NoFinger),
            EnrollReply::Failed(code) | EnrollReply::Unknown(code) => {
                return Err(VeinError::Rc(code))
            }
        }
    }
    if !enrolled {
        // 途中経過が上限まで続いた = 応答の破損
        return Err(VeinError::ReadFail);
    }

    // 4. 登録データの大きさ
    let p = exchange(port, t, CMD_READ_ENROLL, &id)?;
    if p.data[0] != XG_ERR_SUCCESS {
        return Err(VeinError::Rc(p.data[1]));
    }
    let size = usize::from(p.data[1]) + usize::from(p.data[2]) * 256;
    if !(1..=CHARA_MAX).contains(&size) {
        return Err(VeinError::ReadFail);
    }

    // 5. 分割読み出し (種別 = READ_ENROLL)。塊ごとに和を確かめ、合わなければ読み直す
    let mut out = Vec::with_capacity(size);
    let mut buf = vec![0u8; UART_DATA_PACKET_MAX + 2];
    for (offset, len) in chunks(size) {
        let mut ok = false;
        for _ in 0..READ_RETRIES {
            port.discard_input();
            let req = read_data_request(CMD_READ_ENROLL, offset as u32, len as u32);
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

    // 6. 作業用 ID を消して終わる。失敗しても読めた分は返す (次の読み取りの 2. で消える)
    let _ = exchange(port, t, CMD_CLEAR_ENROLL, &id);
    Ok(out)
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
        let p = read_data_request(CMD_READ_ENROLL, 0x200, 0x48);
        assert_eq!(p[3], CMD_READ_DATA);
        assert_eq!(p[5], 9);
        assert_eq!(&p[6..15], &[0x22, 0x00, 0x02, 0, 0, 0x48, 0x00, 0, 0]);
        assert!(parse_packet(&p).is_ok());
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
}
