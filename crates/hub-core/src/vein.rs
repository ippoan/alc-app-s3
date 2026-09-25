//! 指静脈モジュール (Waveshare Finger Vein Scanner Module、XG 系) の UART
//! プロトコル — **純粋部分** (ippoan/vein-match#20)。
//!
//! Vein Station (Atom VoiceS3R + `atoms3-timecard` の `vein` feature) が
//! モジュールから特徴量を取り出し、`VEIN CHARA <hex>` 行でホスト (alc-app の
//! Web Serial) へ渡すための手順。UART の読み書きは [`Port`] の向こう
//! (`alc-hub-drivers::vein`) にあり、ここはパケットの組み立て・検査と
//! 「接続 → 撮像 → 分割読み出し」の進め方だけを持つ (ホストで `cargo test`)。
//!
//! # 出典 (机上。**実機では未確認**)
//!
//! 仕様書 Communication Protocol Ver 2.3 (§1.2 パケット・§1.2.6 分割読み出し・
//! §2.2.1 接続・§2.2.28 GET_CHARA・付録のエラーコード) と、その机上調査
//! (`finger-vein/report.md` §5・§7)。実機が届いたら最初に疑う点:
//!
//! - **READ_DATA の応答は「生データ + 2 バイトの和」だけ**で、応答パケットを
//!   前に挟まない (§1.2.6 のサンプルがそう読む)
//! - **特徴量の大きさは `bData[1] + bData[2] * 256`**。§2.2.28 のサンプルは
//!   `* 255` だが誤記と見る (report.md §5 の注、付録 5.4 のテンプレートサイズ)
//! - 特徴量の中身 (0xBDBD で始まる 0x448 バイトか、外側の層か) は**ここでは
//!   解釈しない** — モジュールが返した大きさのまま 16 進で出す
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
/// 撮像して特徴量を作る。読み出しは READ_DATA の種別 0x28 (§2.2.28)
pub const CMD_GET_CHARA: u8 = 0x28;

/// 工場出荷時の接続パスワード ("0" × 8、§2.2.1)
pub const DEFAULT_PASSWORD: &[u8; 8] = b"00000000";

/// 成功
pub const XG_ERR_SUCCESS: u8 = 0x00;
/// 失敗 (`bData[1]` が理由のエラーコード)
pub const XG_ERR_FAIL: u8 = 0x01;
/// 指の入力待ちがモジュール側で時間切れ
pub const XG_ERR_TIME_OUT: u8 = 0x0B;
/// 「指を置いて」の途中経過 (GET_CHARA 中に届く)
pub const XG_INPUT_FINGER: u8 = 0x20;
/// 「指を離して」の途中経過 (GET_CHARA 中に届く)
pub const XG_RELEASE_FINGER: u8 = 0x21;

/// READ_DATA の 1 回の上限 (**UART は 512 バイト**。USB は 4096、§1.2.6)
pub const UART_DATA_PACKET_MAX: usize = 512;
/// 特徴量として受け取る大きさの上限。DLL の読み込みが受け付ける上限
/// (`len > 0x7d0` で err 3) に合わせる — これを超える値は応答の破損とみなす
pub const CHARA_MAX: usize = 0x7d0;

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

/// GET_CHARA 中に届いた応答の意味 (§2.2.28)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharaReply {
    /// 撮像できた。特徴量の大きさ (バイト)
    Ready(usize),
    /// 途中経過 (「置いて」/「離して」)。次の応答を待つ
    Prompt,
    /// 失敗。`bData[1]` のエラーコード
    Failed(u8),
    /// 知らない `bData[0]`
    Unknown(u8),
}

/// GET_CHARA の応答を読む。大きさは `bData[1] + bData[2] * 256`
/// (サンプルの `* 255` は誤記と見る — モジュールの冒頭 doc)
pub fn chara_reply(p: &Packet) -> CharaReply {
    match p.data[0] {
        XG_ERR_SUCCESS => CharaReply::Ready(usize::from(p.data[1]) + usize::from(p.data[2]) * 256),
        XG_ERR_FAIL => CharaReply::Failed(p.data[1]),
        XG_INPUT_FINGER | XG_RELEASE_FINGER => CharaReply::Prompt,
        other => CharaReply::Unknown(other),
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
    /// 接続の応答を待つ時間。これで来なければ [`VeinError::NoModule`]
    pub connect_ms: u32,
    /// GET_CHARA の応答 (途中経過を含む) 1 つを待つ時間。指を置くまでの
    /// 待ちはモジュール側が持つので、それより長くとる
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
/// GET_CHARA 中に途中経過を受け付ける回数の上限 (応答が途切れず続く故障で
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

/// 接続して撮像し、特徴量を読み出す (§2.2.1 → §2.2.28 → §1.2.6)。
///
/// 戻り値はモジュールが返した大きさそのままのバイト列 (中身は解釈しない)。
/// 案内音声は鳴らさない — いつ何を鳴らすかはホストが `VEIN SAY` で決める
pub fn capture(port: &mut dyn Port, t: &Timeouts) -> Result<Vec<u8>, VeinError> {
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

    // 2. 撮像。途中経過 (置いて / 離して) を読み流し、大きさが来るまで待つ
    if !port.send(&command(CMD_GET_CHARA, &[])) {
        return Err(VeinError::ReadFail);
    }
    let mut size = None;
    for _ in 0..=MAX_PROMPTS {
        let p = recv_packet(port, t.finger_ms, t.idle_ms).map_err(|e| match e {
            RecvError::Silent => VeinError::Timeout,
            RecvError::Broken => VeinError::ReadFail,
        })?;
        if p.cmd != CMD_GET_CHARA {
            return Err(VeinError::ReadFail);
        }
        match chara_reply(&p) {
            CharaReply::Ready(n) => {
                size = Some(n);
                break;
            }
            CharaReply::Prompt => continue,
            CharaReply::Failed(XG_ERR_TIME_OUT) => return Err(VeinError::NoFinger),
            CharaReply::Failed(code) | CharaReply::Unknown(code) => {
                return Err(VeinError::Rc(code))
            }
        }
    }
    let size = match size {
        Some(n) if (1..=CHARA_MAX).contains(&n) => n,
        // 途中経過が上限まで続いた / 大きさが 0 や上限超え = 応答の破損
        _ => return Err(VeinError::ReadFail),
    };

    // 3. 分割読み出し (種別 = GET_CHARA)。塊ごとに和を確かめ、合わなければ読み直す
    let mut out = Vec::with_capacity(size);
    let mut buf = vec![0u8; UART_DATA_PACKET_MAX + 2];
    for (offset, len) in chunks(size) {
        let mut ok = false;
        for _ in 0..READ_RETRIES {
            port.discard_input();
            let req = read_data_request(CMD_GET_CHARA, offset as u32, len as u32);
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

    fn ready(size: usize) -> Option<Vec<u8>> {
        Some(reply(CMD_GET_CHARA, &[0x00, size as u8, (size >> 8) as u8]))
    }

    /// 0xBDBD で始まる 0x448 バイトの見本 (中身は解釈しないので形だけ)
    fn sample_chara() -> Vec<u8> {
        let mut v: Vec<u8> = (0..0x448).map(|i| (i * 7 + 3) as u8).collect();
        v[0] = 0xBD;
        v[1] = 0xBD;
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
    fn get_chara_packet_has_no_data() {
        let p = command(CMD_GET_CHARA, &[]);
        assert_eq!(&p[..6], &[0xBB, 0xAA, 0x00, 0x28, 0x00, 0x00]);
        assert_eq!(u16::from_le_bytes([p[22], p[23]]), 0xBB + 0xAA + 0x28);
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
        let p = read_data_request(CMD_GET_CHARA, 0x200, 0x48);
        assert_eq!(p[3], CMD_READ_DATA);
        assert_eq!(p[5], 9);
        assert_eq!(&p[6..15], &[0x28, 0x00, 0x02, 0, 0, 0x48, 0x00, 0, 0]);
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
    }

    #[test]
    fn chara_reply_kinds() {
        let pk = |d: &[u8]| parse_packet(&reply(CMD_GET_CHARA, d).try_into().unwrap()).unwrap();
        // * 256 (サンプルの * 255 ではない)
        assert_eq!(
            chara_reply(&pk(&[0x00, 0x48, 0x04])),
            CharaReply::Ready(0x448)
        );
        assert_eq!(chara_reply(&pk(&[XG_INPUT_FINGER])), CharaReply::Prompt);
        assert_eq!(chara_reply(&pk(&[XG_RELEASE_FINGER])), CharaReply::Prompt);
        assert_eq!(
            chara_reply(&pk(&[XG_ERR_FAIL, 0x0B])),
            CharaReply::Failed(0x0B)
        );
        assert_eq!(chara_reply(&pk(&[0x42])), CharaReply::Unknown(0x42));
    }

    #[test]
    fn reasons_and_lines() {
        assert_eq!(err_line(VeinError::NoModule), "ERR VEIN NO_MODULE");
        assert_eq!(err_line(VeinError::Timeout), "ERR VEIN TIMEOUT");
        assert_eq!(err_line(VeinError::NoFinger), "ERR VEIN NO_FINGER");
        assert_eq!(err_line(VeinError::ReadFail), "ERR VEIN READ_FAIL");
        assert_eq!(err_line(VeinError::Rc(0x0C)), "ERR VEIN RC=0C");
        assert_eq!(chara_line(&[0x0A, 0xBD]), "VEIN CHARA 0ABD");
        assert_eq!(chara_line(&sample_chara()).len(), 11 + 2192);
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
    fn capture_happy_path_reads_three_chunks() {
        let chara = sample_chara();
        // 途中経過 (置いて / 離して) と大きさは GET_CHARA 1 回の送信に続けて届く
        let mut get_chara = reply(CMD_GET_CHARA, &[XG_INPUT_FINGER]);
        get_chara.extend(reply(CMD_GET_CHARA, &[XG_RELEASE_FINGER]));
        get_chara.extend(ready(chara.len()).unwrap());
        let mut script = vec![connected(), Some(get_chara)];
        for (off, len) in chunks(chara.len()) {
            script.push(Some(chunk(&chara[off..off + len])));
        }
        let mut port = FakePort::new(script);
        assert_eq!(capture(&mut port, &Timeouts::DEFAULT), Ok(chara));
        // 接続 → GET_CHARA → READ_DATA ×3
        assert_eq!(port.sent.len(), 5);
        assert_eq!(port.sent[0], SPEC_CONNECT_SEND.to_vec());
        assert_eq!(port.sent[1][3], CMD_GET_CHARA);
        assert_eq!(
            &port.sent[4][6..15],
            &[0x28, 0x00, 0x04, 0, 0, 0x48, 0, 0, 0]
        );
    }

    #[test]
    fn capture_skips_garbage_before_prefix() {
        let data = vec![0xBD, 0xBD, 1, 2];
        let mut noisy = vec![0x00, 0xBB, 0x13];
        noisy.extend(SPEC_CONNECT_RECV);
        let mut port = FakePort::new(vec![Some(noisy), ready(4), Some(chunk(&data))]);
        assert_eq!(capture(&mut port, &Timeouts::DEFAULT), Ok(data));
    }

    #[test]
    fn capture_retries_bad_chunk() {
        let data = vec![9u8; 10];
        let mut bad = chunk(&data);
        bad[0] ^= 0xFF;
        let mut port = FakePort::new(vec![
            connected(),
            ready(10),
            Some(bad),
            None,
            Some(chunk(&data)),
        ]);
        assert_eq!(capture(&mut port, &Timeouts::DEFAULT), Ok(data));
        // 読み直しの前に毎回残りを捨てる (接続前 1 + 塊 3 回)
        assert_eq!(port.discards, 4);
    }

    #[test]
    fn capture_gives_up_after_retries() {
        let mut port = FakePort::new(vec![connected(), ready(10), None, None, None]);
        assert_eq!(
            capture(&mut port, &Timeouts::DEFAULT),
            Err(VeinError::ReadFail)
        );
        assert_eq!(port.sent.len(), 2 + READ_RETRIES);
    }

    #[test]
    fn capture_chunk_cut_short_is_retried_then_fails() {
        let data = vec![1u8; 10];
        let short = chunk(&data)[..5].to_vec();
        let mut port = FakePort::new(vec![
            connected(),
            ready(10),
            Some(short.clone()),
            Some(short.clone()),
            Some(short),
        ]);
        assert_eq!(
            capture(&mut port, &Timeouts::DEFAULT),
            Err(VeinError::ReadFail)
        );
    }

    #[test]
    fn capture_no_module_when_silent() {
        let mut port = FakePort::new(vec![None]);
        assert_eq!(
            capture(&mut port, &Timeouts::DEFAULT),
            Err(VeinError::NoModule)
        );
    }

    #[test]
    fn capture_no_module_when_send_fails() {
        let mut port = FakePort::new(vec![]);
        port.send_ok = false;
        assert_eq!(
            capture(&mut port, &Timeouts::DEFAULT),
            Err(VeinError::NoModule)
        );
    }

    #[test]
    fn capture_connect_errors() {
        // 途中で途絶えた接続応答
        let mut port = FakePort::new(vec![Some(SPEC_CONNECT_RECV[..10].to_vec())]);
        assert_eq!(
            capture(&mut port, &Timeouts::DEFAULT),
            Err(VeinError::ReadFail)
        );
        // 別コマンドの応答
        let mut port = FakePort::new(vec![Some(reply(0x02, &[0]))]);
        assert_eq!(
            capture(&mut port, &Timeouts::DEFAULT),
            Err(VeinError::ReadFail)
        );
        // パスワード違い (XG_ERR_INVALID_PWD = 0x04)
        let mut port = FakePort::new(vec![Some(reply(CMD_CONNECTION, &[XG_ERR_FAIL, 0x04]))]);
        assert_eq!(
            capture(&mut port, &Timeouts::DEFAULT),
            Err(VeinError::Rc(0x04))
        );
        // 和が合わない
        let mut bad = SPEC_CONNECT_RECV.to_vec();
        bad[8] ^= 1;
        let mut port = FakePort::new(vec![Some(bad)]);
        assert_eq!(
            capture(&mut port, &Timeouts::DEFAULT),
            Err(VeinError::ReadFail)
        );
        // 1 バイトだけ届いて途絶えた (識別子の 2 バイト目が来ない)
        let mut port = FakePort::new(vec![Some(vec![0xBB])]);
        assert_eq!(
            capture(&mut port, &Timeouts::DEFAULT),
            Err(VeinError::ReadFail)
        );
        // 識別子が見つからないまま前置きが長すぎる
        let mut port = FakePort::new(vec![Some(vec![0x11; MAX_GARBAGE + 3])]);
        assert_eq!(
            capture(&mut port, &Timeouts::DEFAULT),
            Err(VeinError::ReadFail)
        );
    }

    #[test]
    fn capture_get_chara_errors() {
        let run = |second: Option<Vec<u8>>| {
            let mut port = FakePort::new(vec![connected(), second]);
            capture(&mut port, &Timeouts::DEFAULT)
        };
        assert_eq!(run(None), Err(VeinError::Timeout));
        assert_eq!(
            run(Some(reply(CMD_GET_CHARA, &[XG_ERR_FAIL, XG_ERR_TIME_OUT]))),
            Err(VeinError::NoFinger)
        );
        assert_eq!(
            run(Some(reply(CMD_GET_CHARA, &[XG_ERR_FAIL, 0x11]))),
            Err(VeinError::Rc(0x11))
        );
        assert_eq!(
            run(Some(reply(CMD_GET_CHARA, &[0x42]))),
            Err(VeinError::Rc(0x42))
        );
        assert_eq!(
            run(Some(reply(CMD_CONNECTION, &[0]))),
            Err(VeinError::ReadFail)
        );
        assert_eq!(
            run(Some(SPEC_CONNECT_RECV[..3].to_vec())),
            Err(VeinError::ReadFail)
        );
        // 大きさ 0 / 上限超え
        assert_eq!(run(ready(0)), Err(VeinError::ReadFail));
        assert_eq!(run(ready(CHARA_MAX + 1)), Err(VeinError::ReadFail));
    }

    #[test]
    fn capture_endless_prompts_is_read_fail() {
        let prompts: Vec<u8> = (0..=MAX_PROMPTS)
            .flat_map(|_| reply(CMD_GET_CHARA, &[XG_INPUT_FINGER]))
            .collect();
        let mut port = FakePort::new(vec![connected(), Some(prompts)]);
        assert_eq!(
            capture(&mut port, &Timeouts::DEFAULT),
            Err(VeinError::ReadFail)
        );
    }

    #[test]
    fn capture_get_chara_send_fails() {
        struct FailSecond(FakePort);
        impl Port for FailSecond {
            fn send(&mut self, b: &[u8]) -> bool {
                self.0.send(b) && self.0.sent.len() < 2
            }
            fn discard_input(&mut self) {
                self.0.discard_input()
            }
            fn recv(&mut self, buf: &mut [u8], t: u32) -> usize {
                self.0.recv(buf, t)
            }
        }
        let mut port = FailSecond(FakePort::new(vec![connected()]));
        assert_eq!(
            capture(&mut port, &Timeouts::DEFAULT),
            Err(VeinError::ReadFail)
        );
    }

    #[test]
    fn capture_read_data_send_fails_is_retried() {
        struct FailThird(FakePort);
        impl Port for FailThird {
            fn send(&mut self, b: &[u8]) -> bool {
                self.0.send(b) && self.0.sent.len() != 3
            }
            fn discard_input(&mut self) {
                self.0.discard_input()
            }
            fn recv(&mut self, buf: &mut [u8], t: u32) -> usize {
                self.0.recv(buf, t)
            }
        }
        let data = vec![5u8; 3];
        let mut port = FailThird(FakePort::new(vec![
            connected(),
            ready(3),
            None,
            Some(chunk(&data)),
        ]));
        assert_eq!(capture(&mut port, &Timeouts::DEFAULT), Ok(data));
    }
}
