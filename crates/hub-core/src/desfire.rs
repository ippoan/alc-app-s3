//! DESFire native APDU のコーデック (Refs ippoan/alc-app-s3#110)。
//!
//! 電子車検証の IC は DESFire で、PIN 不要で読める「電子車検証管理番号」を
//! File 03 から取る。手順 (アプリを選び、File 03 を読む) は `hub-drivers` の
//! `nfc.rs` (`read_carins_mgmt`) にあり、ここはコマンドの組み立てと応答の
//! 読み解きだけを持つ。構造 (AID・File 03 が平文で読めること・中身の形) は
//! #234 の計器で実機を測って確かめた。
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

/// 登録車 (普通車) の電子車検証アプリの AID。実機で SELECT → File 03 の読みを確認済み
pub const AID_REGISTERED: [u8; 3] = [0xF3, 0x30, 0x11];
/// 軽自動車の電子車検証アプリの AID (登録車で選べなかったときに試す)。実機は未確認
pub const AID_KEI: [u8; 3] = [0xF3, 0x30, 0x18];
/// 車両 ID と電子車検証管理番号が入っているファイル番号 (平文・鍵なしで読める)
pub const FILE_NO_MGMT: u8 = 0x03;

const CLA_NATIVE: u8 = 0x90;
const INS_SELECT_APPLICATION: u8 = 0x5A;
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

/// `SelectApplication` — 以後のコマンドの対象アプリを選ぶ。
/// **選択はセッション (活性化) に紐づく**ので、後続と同じセッションで送ること
pub fn select_application(aid: [u8; 3]) -> Vec<u8> {
    wrap_native(INS_SELECT_APPLICATION, &aid)
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

/// [`parse_mgmt_record`] の失敗の理由 (`EVT NFC_CARINS rc=` に載る)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// パディングを落とすと空 (値は落とした後の長さ = 0)
    Length(usize),
    /// UTF-8 として読めない
    NotUtf8,
    /// `/` 区切りが無い
    NoSeparator,
    /// 区切りの片側が空
    EmptyField,
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

/// File 03 の中身。実機の形 (#234 の計器): UTF-8 の `車両ID/電子車検証管理番号` を
/// `/` で区切り、残りを 0 で埋めたもの。車両 ID は英数 14 桁、管理番号は数字 12 桁
/// (軽は 13 桁)。**値の妥当性 (桁・文字種) はここでは見ない** — 受け取るサーバが検査する
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MgmtRecord {
    /// `/` の前 (車両 ID)
    pub vehicle_id: String,
    /// `/` の後 (電子車検証管理番号)
    pub cert_no: String,
}

/// File 03 の中身を [`MgmtRecord`] に読む。
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
        assert_eq!(
            select_application(AID_REGISTERED),
            vec![0x90, 0x5A, 0x00, 0x00, 0x03, 0xF3, 0x30, 0x11, 0x00]
        );
        assert_eq!(select_application(AID_KEI)[7], 0x18);
        assert_eq!(additional_frame(), vec![0x90, 0xAF, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn read_data_encodes_le24_offset_and_len() {
        // file 03 / offset 0 / len 0 (= 全体)
        assert_eq!(
            read_data(FILE_NO_MGMT, 0, 0),
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

    // 値はすべて合成値 (実在の車両 ID・管理番号を書かない)
    const SYNTH: &[u8] = b"TESTCARID00001/000000000001";

    fn padded(fill: u8, len: usize) -> Vec<u8> {
        let mut v = SYNTH.to_vec();
        v.resize(len, fill);
        v
    }

    fn synth_record() -> MgmtRecord {
        MgmtRecord { vehicle_id: "TESTCARID00001".into(), cert_no: "000000000001".into() }
    }

    #[test]
    fn parse_mgmt_record_zero_padded() {
        assert_eq!(parse_mgmt_record(&padded(0x00, 64)), Ok(synth_record()));
    }

    #[test]
    fn parse_mgmt_record_ff_padded() {
        assert_eq!(parse_mgmt_record(&padded(0xFF, 64)), Ok(synth_record()));
    }

    #[test]
    fn parse_mgmt_record_unpadded_and_kei_length() {
        assert_eq!(parse_mgmt_record(SYNTH), Ok(synth_record()));
        let kei = parse_mgmt_record(b"TESTCARID00001/0000000000001\x00\x00").unwrap();
        assert_eq!(kei.cert_no, "0000000000001");
    }

    #[test]
    fn parse_mgmt_record_splits_on_first_slash_and_trims() {
        let rec = parse_mgmt_record(b" TESTCARID00001 / 000000000001 \x00\xFF").unwrap();
        assert_eq!(rec, synth_record());
        let rec = parse_mgmt_record(b"TESTCARID00001/000000000001/X").unwrap();
        assert_eq!(rec.cert_no, "000000000001/X");
    }

    #[test]
    fn parse_mgmt_record_without_slash() {
        assert_eq!(
            parse_mgmt_record(b"TESTCARID00001000000000001\x00"),
            Err(ParseError::NoSeparator)
        );
    }

    #[test]
    fn parse_mgmt_record_empty_fields() {
        assert_eq!(parse_mgmt_record(b"/000000000001\x00"), Err(ParseError::EmptyField));
        assert_eq!(parse_mgmt_record(b"TESTCARID00001/ \x00"), Err(ParseError::EmptyField));
    }

    #[test]
    fn parse_mgmt_record_blank_or_broken() {
        assert_eq!(parse_mgmt_record(&[]), Err(ParseError::Length(0)));
        assert_eq!(parse_mgmt_record(&[0x00; 32]), Err(ParseError::Length(0)));
        assert_eq!(parse_mgmt_record(&[0xFF, 0x00, 0xFF]), Err(ParseError::Length(0)));
        assert_eq!(parse_mgmt_record(&[0xC3, 0x28]), Err(ParseError::NotUtf8));
    }
}
