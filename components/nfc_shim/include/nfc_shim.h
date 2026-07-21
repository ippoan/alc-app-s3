/*
 * NFC 検証用シム (Unit NFC / ST25R3916, issue #84 + plan/nfc-card-identity.md)。
 * M5UnitUnified (C++) を薄く包み、Rust から extern "C" で呼べるようにする。
 * I2C バスはこちら (C++/M5UnitUnified) 側で所有する (Rust 側は i2c1 を take しない)。
 */
#ifndef ALC_HUB_NFC_SHIM_H
#define ALC_HUB_NFC_SHIM_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/**
 * I2C バスを開き ST25R3916 を初期化する。
 * i2c_port: 0 or 1 (I2C_NUM_0 / I2C_NUM_1)。cores3 内部バス(電源IC/タッチ)と分離するため 1 を渡す。
 * sda_gpio / scl_gpio: DIN Base Port B の GPIO 番号。役割 (どちらが SDA/SCL か) は未確定のため
 * bring-up で入替えて試すこと。
 * 戻り値: 0=成功、非0=失敗 (I2C バスが ST25R3916 から ack を得られない等)。
 */
int nfc_shim_init(int i2c_port, int sda_gpio, int scl_gpio);

/**
 * 交通系IC (Suica/PASMO 等, NFC-F) を1回ポーリングする。
 * 検出した場合 IDm を16進文字列(小文字なし、大文字16桁、終端NUL込み)で out_hex に書き込み、
 * 書き込んだ文字数 (NUL抜き) を返す。未検出は 0、エラーは負値。
 */
int nfc_shim_poll_felica_idm(char* out_hex, int out_cap);

/**
 * NFC-A (Type-A, NTAG21x/MIFARE 等) を1回ポーリングする。UID を16進文字列で
 * out_hex に書き込み、書き込んだ文字数 (NUL抜き) を返す。未検出は 0、エラーは負値。
 * 既知良品カード (スマホの Web NFC で反応確認済み等) での動作切り分け用
 */
int nfc_shim_poll_nfca_uid(char* out_hex, int out_cap);

/**
 * 従来 IC 運転免許証の MF 直下 EF 2F01 (共通データ要素) を PIN なしで読み、
 * 交付日・有効期限を "YYYYMMDD" 形式の文字列で返す (plan/nfc-card-identity.md の
 * BCD デコード規則、ippoan/AlcoholChecker の NfcReader.kt と同じ APDU シーケンス)。
 * 戻り値: 0=成功、非0=失敗 (カード無し/免許証以外/読み取り失敗)。
 */
int nfc_shim_read_license_expiry(char* out_issue, int issue_cap, char* out_expiry, int expiry_cap);

/**
 * Type-A (ISO14443-4/ISO-DEP) の汎用 APDU 送受信 (issue #105)。
 * WUPA→SELECT(RATS 込み)→APDU 送受信→HLTA を1セッションとして行い、
 * ISO14443-4 対応カード (RATS に応答するもの) にのみ動作する。
 * `cmd`/`cmd_len` に送信 APDU、`out`/`out_cap` に受信バッファを渡す —
 * AID 等プロトコル固有のバイト列は Rust 側が組み立てる (C++ 側にはハード
 * コードしない、Plan agent レビュー結果を反映)。
 * `nfc_shim_poll_nfca_uid()` の detect() は毎回 HLTA で終わり ISO-DEP
 * セッションが残らないため、この用途には使えない (新規セッションが必要)。
 * 実機確認の結果、電子車検証はこの Type-A 経路で応答することを確認した
 * (Android 実装 ippoan/AlcoholChecker の一次情報は Type-B 想定だったが
 * 実機の挙動と異なっていた)。
 * 戻り値: >=0 (受信バイト数、SW1SW2 込み) で成功、負値は失敗
 *   (-1=引数不正/未初期化, -2=カード無し, -3=SELECT/RATS 失敗,
 *    -4=ISO14443-4 非対応, -5=APDU 送受信失敗)。
 */
int nfc_shim_transceive_apdu_a(const uint8_t* cmd, int cmd_len, uint8_t* out, int out_cap);

/**
 * アンテナ振幅を1回測定して返す (0-255、失敗時 -1)。カード接近の存在検知
 * 実験用 (issue #96 続き、2026-07-21)
 */
int nfc_shim_measure_amplitude(void);

/**
 * RFO-RFI 位相差を1回測定して返す (0-255、失敗時 -1)。スマホ等の振幅に
 * 出にくい対象の存在検知用 (2026-07-21)
 */
int nfc_shim_measure_phase(void);

#ifdef __cplusplus
}
#endif

#endif
