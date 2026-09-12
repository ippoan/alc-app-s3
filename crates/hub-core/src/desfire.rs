//! DESFire native APDU のコーデック (Refs ippoan/alc-app-s3#110)。
//!
//! 電子車検証の IC は DESFire で、PIN 不要で読める「電子車検証管理番号」を
//! 取るのが最終目的。ただし**カードの構造をまだ実機で測っていない**ため、
//! このモジュールは「実機で構造を測るため」のコマンド組み立てと応答の
//! 読み解きだけを持つ。手順 (どの AID を選び、どのファイルを読むか) は
//! 実測が出るまで固まらないので `hub-drivers` 側の直線コードに置く。
//!
//! # なぜ vendor の `DESFireFileSystem` を使わないか
//!
//! `components/M5Unit-NFC/src/nfc/isoDEP/desfire_file_system.cpp` に同等の
//! 実装が vendor 済みだが、(1) `nfc_shim.h` が「AID 等プロトコル固有のバイト列は
//! Rust 側が組み立てる。C++ にハードコードしない」と定めている (2) vendor 品に
//! プロトコル判断を預けると上流の更新に縛られる (3) **ホストテストが書けるのは
//! Rust 側だけ** — の 3 点による (決定済み)。
//!
//! # native wrap (ISO7816 ラップ)
//!
//! DESFire の native コマンドは `90 <INS> 00 00 [Lc <data>] 00` の形で
//! ISO7816-4 の APDU に包んで送る。応答の末尾 2 バイトは `91 <status>` で、
//! `91 00` = 成功、`91 AF` = 続きあり (`90 AF` を送って継ぎ足す)。

/// 登録車 (普通車) の電子車検証アプリの AID。実機未検証の候補
pub const AID_REGISTERED: [u8; 3] = [0xF3, 0x30, 0x11];
/// 軽自動車の電子車検証アプリの AID。実機未検証の候補
pub const AID_KEI: [u8; 3] = [0xF3, 0x30, 0x18];
/// 管理番号が入っていると想定しているファイル番号。実機未検証の候補
pub const FILE_NO_MGMT: u8 = 0x03;

const CLA_NATIVE: u8 = 0x90;
const INS_GET_APPLICATION_IDS: u8 = 0x6A;
const INS_SELECT_APPLICATION: u8 = 0x5A;
const INS_GET_FILE_IDS: u8 = 0x6F;
const INS_GET_FILE_SETTINGS: u8 = 0xF5;
const INS_READ_DATA: u8 = 0xAD;
const INS_ADDITIONAL_FRAME: u8 = 0xAF;

/// native コマンドを ISO7816 の APDU に包む: `90 INS 00 00 [Lc data] 00`。
/// data が空なら Lc を省く (DESFire は Lc=0 を受け付けない実装がある)
pub fn wrap_native(ins: u8, data: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(6 + data.len());
    v.extend_from_slice(&[CLA_NATIVE, ins, 0x00, 0x00]);
    if !data.is_empty() {
        v.push(data.len() as u8);
        v.extend_from_slice(data);
    }
    v.push(0x00); // Le
    v
}

/// `GetApplicationIDs` — カード上のアプリ (AID) の一覧を返させる
pub fn get_application_ids() -> Vec<u8> {
    wrap_native(INS_GET_APPLICATION_IDS, &[])
}

/// `SelectApplication` — 以後のコマンドの対象アプリを選ぶ。
/// **選択はセッション (活性化) に紐づく**ので、後続と同じセッションで送ること
pub fn select_application(aid: [u8; 3]) -> Vec<u8> {
    wrap_native(INS_SELECT_APPLICATION, &aid)
}

/// `GetFileIDs` — 選択中アプリのファイル番号の一覧
pub fn get_file_ids() -> Vec<u8> {
    wrap_native(INS_GET_FILE_IDS, &[])
}

/// `GetFileSettings` — 1 ファイルの型・通信モード・アクセス権・サイズ
pub fn get_file_settings(file_no: u8) -> Vec<u8> {
    wrap_native(INS_GET_FILE_SETTINGS, &[file_no])
}

/// `ReadData` — `len` = 0 でファイル全体
pub fn read_data(file_no: u8, offset: u32, len: u32) -> Vec<u8> {
    let mut data = [0u8; 7];
    data[0] = file_no;
    data[1..4].copy_from_slice(&le24(offset));
    data[4..7].copy_from_slice(&le24(len));
    wrap_native(INS_READ_DATA, &data)
}

/// `AdditionalFrame` (`90 AF`) — `91 AF` で切れた応答の続きを要求する
pub fn additional_frame() -> Vec<u8> {
    wrap_native(INS_ADDITIONAL_FRAME, &[])
}

fn le24(v: u32) -> [u8; 3] {
    [v as u8, (v >> 8) as u8, (v >> 16) as u8]
}

fn unpack_le24(b: &[u8]) -> u32 {
    u32::from(b[0]) | (u32::from(b[1]) << 8) | (u32::from(b[2]) << 16)
}

/// 応答末尾 2 バイト (`91 xx`) の読み解き
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// `91 00`
    Ok,
    /// `91 AF` — 続きあり
    MoreFrames,
    /// `91 xx` (xx は 00/AF 以外)
    Error(u8),
    /// 末尾が `91` で始まらない = DESFire の native 応答ではない
    NotDesfire([u8; 2]),
    /// 2 バイト未満
    TooShort,
}

/// 応答の末尾 2 バイトから [`Status`] を判定する
pub fn status(rx: &[u8]) -> Status {
    if rx.len() < 2 {
        return Status::TooShort;
    }
    let sw1 = rx[rx.len() - 2];
    let sw2 = rx[rx.len() - 1];
    if sw1 != 0x91 {
        return Status::NotDesfire([sw1, sw2]);
    }
    match sw2 {
        0x00 => Status::Ok,
        INS_ADDITIONAL_FRAME => Status::MoreFrames,
        other => Status::Error(other),
    }
}

/// 応答から末尾 2 バイト (`91 xx`) を落としたデータ部
pub fn payload(rx: &[u8]) -> &[u8] {
    if rx.len() < 2 {
        return &[];
    }
    &rx[..rx.len() - 2]
}

/// コーデックの解釈失敗。**どれも仮説の検証が目的**なので細かく分けて返す
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// 長さが想定と合わない (値は実際の長さ)
    Length(usize),
    /// UTF-8 として読めない
    NotUtf8,
    /// `/` 区切りが無い
    NoSeparator,
    /// 区切りの片側が空
    EmptyField,
}

/// `GetApplicationIDs` の応答データを 3 バイトずつの AID に割る
pub fn parse_application_ids(payload: &[u8]) -> Result<Vec<[u8; 3]>, ParseError> {
    if payload.len() % 3 != 0 {
        return Err(ParseError::Length(payload.len()));
    }
    Ok(payload.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect())
}

/// `GetFileIDs` の応答データ (ファイル番号がそのまま並ぶ)
pub fn parse_file_ids(payload: &[u8]) -> Vec<u8> {
    payload.to_vec()
}

/// `GetFileSettings` の応答
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileSettings {
    /// ファイル型 (00=Standard Data, 01=Backup, 02=Value, 03/04=Record, 05=TransactionMAC)
    pub file_type: u8,
    /// 通信モードのバイト (下位 2 bit が意味を持つ)
    pub comm_mode_raw: u8,
    /// アクセス権 (LE u16。上位ニブルから Read / Write / ReadWrite / Change)
    pub access_rights: u16,
    /// ファイルサイズ (TransactionMAC では 0)
    pub file_size: u32,
}

impl FileSettings {
    /// 0 = 平文 / 1 = MAC / 3 = 暗号
    pub fn comm_mode(&self) -> u8 {
        self.comm_mode_raw & 0x03
    }

    /// 平文で読めるか
    pub fn is_plain(&self) -> bool {
        self.comm_mode() == 0
    }

    /// アクセス権の Read ニブル (最上位)。`0xE` = free (鍵不要)
    pub fn read_key(&self) -> u8 {
        ((self.access_rights >> 12) & 0x0F) as u8
    }

    /// 鍵無しで読めるか
    pub fn is_free_read(&self) -> bool {
        self.read_key() == 0x0E
    }
}

/// `GetFileSettings` の応答データを読む。
/// wire 形式は vendor の `desfire_file_system.cpp` (`getFileSettings`) と同じ解釈:
/// `[type][option][rights LE u16][size le24]`。type 0x05 (TransactionMAC) は
/// 4 バイトで終わり、それ以外は 7 バイト以上
pub fn parse_file_settings(payload: &[u8]) -> Result<FileSettings, ParseError> {
    if payload.len() < 4 {
        return Err(ParseError::Length(payload.len()));
    }
    let file_type = payload[0];
    let comm_mode_raw = payload[1];
    let access_rights = u16::from(payload[2]) | (u16::from(payload[3]) << 8);
    if file_type == 0x05 {
        // TransactionMAC file: サイズを持たない
        return Ok(FileSettings { file_type, comm_mode_raw, access_rights, file_size: 0 });
    }
    if payload.len() < 7 {
        return Err(ParseError::Length(payload.len()));
    }
    Ok(FileSettings {
        file_type,
        comm_mode_raw,
        access_rights,
        file_size: unpack_le24(&payload[4..7]),
    })
}

/// `91 AF` の継ぎ足し 1 回ぶんの結果
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadStep {
    /// 全部揃った (積み上げ済みのデータ)
    Done(Vec<u8>),
    /// `90 AF` を送って続きを取る
    NeedMore,
    /// 失敗 (理由の [`Status`])
    Failed(Status),
}

/// 応答 1 フレームを `acc` へ継ぎ足し、次にすべきことを返す
pub fn accumulate(acc: &mut Vec<u8>, rx: &[u8]) -> ReadStep {
    let st = status(rx);
    match st {
        Status::Ok => {
            acc.extend_from_slice(payload(rx));
            ReadStep::Done(acc.clone())
        }
        Status::MoreFrames => {
            acc.extend_from_slice(payload(rx));
            ReadStep::NeedMore
        }
        other => ReadStep::Failed(other),
    }
}

/// 管理番号レコードの仮説: UTF-8 の `車両ID / 電子車検証管理番号` を `/` で区切ったもの
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MgmtRecord {
    /// `/` の前
    pub vehicle_id: String,
    /// `/` の後
    pub cert_no: String,
}

/// [`MgmtRecord`] の仮説で読んでみる。**未検証の仮説**なので失敗の理由を細かく返す。
/// 末尾の 0x00 / 0xFF パディングを落とし、UTF-8 として読み、最初の `/` で 2 分割する
pub fn parse_mgmt_record(payload: &[u8]) -> Result<MgmtRecord, ParseError> {
    let end = payload
        .iter()
        .rposition(|&b| b != 0x00 && b != 0xFF)
        .map_or(0, |i| i + 1);
    let trimmed = &payload[..end];
    if trimmed.is_empty() {
        return Err(ParseError::Length(0));
    }
    let s = core::str::from_utf8(trimmed).map_err(|_| ParseError::NotUtf8)?;
    let (a, b) = s.split_once('/').ok_or(ParseError::NoSeparator)?;
    let vehicle_id = a.trim();
    let cert_no = b.trim();
    if vehicle_id.is_empty() || cert_no.is_empty() {
        return Err(ParseError::EmptyField);
    }
    Ok(MgmtRecord { vehicle_id: vehicle_id.to_string(), cert_no: cert_no.to_string() })
}

/// 大文字 hex 文字列 (区切り無し)
pub fn hex_upper(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02X}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_native_omits_lc_when_no_data() {
        assert_eq!(wrap_native(0x6A, &[]), vec![0x90, 0x6A, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn wrap_native_includes_lc_and_le() {
        assert_eq!(
            wrap_native(0x5A, &[0x01, 0x02, 0x03]),
            vec![0x90, 0x5A, 0x00, 0x00, 0x03, 0x01, 0x02, 0x03, 0x00]
        );
    }

    #[test]
    fn command_builders() {
        assert_eq!(get_application_ids(), vec![0x90, 0x6A, 0x00, 0x00, 0x00]);
        assert_eq!(
            select_application(AID_REGISTERED),
            vec![0x90, 0x5A, 0x00, 0x00, 0x03, 0xF3, 0x30, 0x11, 0x00]
        );
        assert_eq!(select_application(AID_KEI)[7], 0x18);
        assert_eq!(get_file_ids(), vec![0x90, 0x6F, 0x00, 0x00, 0x00]);
        assert_eq!(
            get_file_settings(FILE_NO_MGMT),
            vec![0x90, 0xF5, 0x00, 0x00, 0x01, 0x03, 0x00]
        );
        assert_eq!(additional_frame(), vec![0x90, 0xAF, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn read_data_encodes_le24_offset_and_len() {
        // file 03 / offset 0 / len 0 (= 全体)
        assert_eq!(
            read_data(0x03, 0, 0),
            vec![0x90, 0xAD, 0x00, 0x00, 0x07, 0x03, 0, 0, 0, 0, 0, 0, 0x00]
        );
        // le24 の 3 バイトが並ぶこと (0x010203 -> 03 02 01)
        let cmd = read_data(0x01, 0x01_02_03, 0x04_05_06);
        assert_eq!(&cmd[6..9], &[0x03, 0x02, 0x01]);
        assert_eq!(&cmd[9..12], &[0x06, 0x05, 0x04]);
    }

    #[test]
    fn status_covers_every_arm() {
        assert_eq!(status(&[0x91, 0x00]), Status::Ok);
        assert_eq!(status(&[0xAA, 0x91, 0xAF]), Status::MoreFrames);
        assert_eq!(status(&[0x91, 0x1C]), Status::Error(0x1C));
        assert_eq!(status(&[0x90, 0x00]), Status::NotDesfire([0x90, 0x00]));
        assert_eq!(status(&[0x91]), Status::TooShort);
        assert_eq!(status(&[]), Status::TooShort);
    }

    #[test]
    fn payload_drops_status_bytes() {
        assert_eq!(payload(&[0xDE, 0xAD, 0x91, 0x00]), &[0xDE, 0xAD]);
        assert_eq!(payload(&[0x91, 0x00]), &[] as &[u8]);
        assert_eq!(payload(&[0x91]), &[] as &[u8]);
    }

    #[test]
    fn parse_application_ids_splits_by_three() {
        assert_eq!(
            parse_application_ids(&[0xF3, 0x30, 0x11, 0xF3, 0x30, 0x18]),
            Ok(vec![AID_REGISTERED, AID_KEI])
        );
        assert_eq!(parse_application_ids(&[]), Ok(vec![]));
        assert_eq!(parse_application_ids(&[0x01, 0x02]), Err(ParseError::Length(2)));
    }

    #[test]
    fn parse_file_ids_passes_through() {
        assert_eq!(parse_file_ids(&[0x01, 0x02, 0x03]), vec![0x01, 0x02, 0x03]);
        assert_eq!(parse_file_ids(&[]), Vec::<u8>::new());
    }

    #[test]
    fn parse_file_settings_standard_file() {
        // type=00 / option=00 (平文) / rights=EEEE / size=0x000020
        let fs = parse_file_settings(&[0x00, 0x00, 0xEE, 0xEE, 0x20, 0x00, 0x00]).unwrap();
        assert_eq!(fs.file_type, 0x00);
        assert_eq!(fs.comm_mode(), 0);
        assert!(fs.is_plain());
        assert_eq!(fs.access_rights, 0xEEEE);
        assert_eq!(fs.read_key(), 0x0E);
        assert!(fs.is_free_read());
        assert_eq!(fs.file_size, 0x20);
    }

    #[test]
    fn parse_file_settings_encrypted_and_keyed() {
        // option=03 (暗号) / rights=0x1234 → Read ニブルは 0x1
        let fs = parse_file_settings(&[0x00, 0x03, 0x34, 0x12, 0x01, 0x00, 0x00]).unwrap();
        assert_eq!(fs.comm_mode(), 3);
        assert!(!fs.is_plain());
        assert_eq!(fs.read_key(), 0x1);
        assert!(!fs.is_free_read());
        // MAC (1) も平文ではない
        let mac = parse_file_settings(&[0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00]).unwrap();
        assert_eq!(mac.comm_mode(), 1);
        assert!(!mac.is_plain());
    }

    #[test]
    fn parse_file_settings_transaction_mac_is_four_bytes() {
        let fs = parse_file_settings(&[0x05, 0x00, 0xEE, 0xEE]).unwrap();
        assert_eq!(fs.file_type, 0x05);
        assert_eq!(fs.file_size, 0);
    }

    #[test]
    fn parse_file_settings_rejects_short() {
        assert_eq!(parse_file_settings(&[0x00, 0x00, 0x00]), Err(ParseError::Length(3)));
        // TransactionMAC 以外で 4..7 バイト
        assert_eq!(
            parse_file_settings(&[0x00, 0x00, 0xEE, 0xEE, 0x20]),
            Err(ParseError::Length(5))
        );
    }

    #[test]
    fn accumulate_joins_frames_until_ok() {
        let mut acc = Vec::new();
        assert_eq!(accumulate(&mut acc, &[0x01, 0x02, 0x91, 0xAF]), ReadStep::NeedMore);
        assert_eq!(
            accumulate(&mut acc, &[0x03, 0x91, 0x00]),
            ReadStep::Done(vec![0x01, 0x02, 0x03])
        );
    }

    #[test]
    fn accumulate_reports_failure() {
        let mut acc = Vec::new();
        assert_eq!(
            accumulate(&mut acc, &[0x91, 0x1C]),
            ReadStep::Failed(Status::Error(0x1C))
        );
        assert!(acc.is_empty());
    }

    #[test]
    fn parse_mgmt_record_splits_on_first_slash() {
        let rec = parse_mgmt_record(b"1234567/890123456789").unwrap();
        assert_eq!(rec.vehicle_id, "1234567");
        assert_eq!(rec.cert_no, "890123456789");
        // 末尾パディングと両側の空白は落とす
        let rec = parse_mgmt_record(b" 12 / 34 \x00\x00\xFF").unwrap();
        assert_eq!(rec.vehicle_id, "12");
        assert_eq!(rec.cert_no, "34");
    }

    #[test]
    fn parse_mgmt_record_error_arms() {
        assert_eq!(parse_mgmt_record(&[]), Err(ParseError::Length(0)));
        assert_eq!(parse_mgmt_record(&[0x00, 0xFF]), Err(ParseError::Length(0)));
        assert_eq!(parse_mgmt_record(&[0xC3, 0x28]), Err(ParseError::NotUtf8));
        assert_eq!(parse_mgmt_record(b"noslash"), Err(ParseError::NoSeparator));
        assert_eq!(parse_mgmt_record(b"/12"), Err(ParseError::EmptyField));
        assert_eq!(parse_mgmt_record(b"12/ "), Err(ParseError::EmptyField));
    }

    #[test]
    fn hex_upper_formats_uppercase() {
        assert_eq!(hex_upper(&[0x0A, 0xF3, 0x00]), "0AF300");
        assert_eq!(hex_upper(&[]), "");
    }
}
