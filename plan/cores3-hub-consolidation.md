# CoreS3 統合ハブ — モジュール配線一次情報 (RS232M / LAN 13.2)

M5Stack CoreS3 に RS232M Module 13.2 と LAN Module 13.2 (W5500/PoE) をスタックして
ハブ化する際の GPIO 割当・ジャンパ/DIP 設定の一次情報。`hub-drivers/src/rs232.rs` /
`hub-drivers/src/lan.rs` のコメントが本ファイルを参照する。

一次情報:

- RS232 サンプル: https://github.com/m5stack/M5Stack/blob/master/examples/Modules/RS232/RS232.ino
- LAN ライブラリ: https://github.com/m5stack/M5Module-LAN-13.2
- LAN Module 公式 doc: https://docs.m5stack.com/en/module/LAN%20Module%2013.2
- **スタック互換ツール (CoreS3=K128 + RS232=M131 + LAN=M136)**:
  https://docs.m5stack.switch-science.com/ja/compatible_stack?host=K128&module=M131%2CM136
- 引継ぎ issue: ippoan/alc-app#102

> ★重要な前提: **モジュールの DIP/ジャンパのシルク番号は「M-Bus のバスピン番号 (無印
> Core 基準)」であり、CoreS3 の実 GPIO 番号ではない** (M5 Community #5581 の
> 「シルク≠GPIO」)。実 GPIO は上記スタック互換ツールが CoreS3 用に翻訳した値を正とする。

## RS232M Module 13.2 → DB9 → FC-1200

UART1 パススルー (`rs232.rs` 実装済み、TX=G17 / RX=G18)。

### DIP 設定 (FC-1200B の測定フロー完走まで実機確認済み 2026-07-15)

モジュールには **RXD 用 / TXD 用の 2 つの DIP (各 5 スイッチ)** があり、各スイッチの
シルク番号は **バスピン番号 (無印 Core の GPIO)**。DIP のバンク名は ESP 視点
(RXD バンク = ESP が受信する線 / TXD バンク = ESP が送信する線)。無印 Core の
公式 RS232.ino が RXD=16 / TXD=17 + `Serial2(RX=16, TX=17)` で動くことと整合する。

| DIP | ON にするスイッチ | シルク (バスpin) | CoreS3 GPIO | 意味 |
|---|---|---|---|---|
| **RXD** | **2 番** | 16 (無印 RXD2) | **G18** | モジュール TX → ホスト RX |
| **TXD** | **2 番** | 17 (無印 TXD2) | **G17** | ホスト TX → モジュール RX |

- RXD シルク並び: `3 / 16 / 13 / 34 / 35` (スイッチ 1〜5) → **2 番 (16)** を ON
- TXD シルク並び: `1 / 17 / 15 / 12 / 0` (スイッチ 1〜5) → **2 番 (17)** を ON
- **他のスイッチは全 OFF**。`ON` 印字のある側へスライダを倒すと ON。
- これで `rs232.rs` の TX=G17 / RX=G18 と一致する (**ファーム変更不要**)。
- ★過去の誤り (本ドキュメント旧版): 「TXD **3 番** (シルク 15) → G18」としていたが、
  **バスpin 15 の CoreS3 翻訳は G13** (LAN CS ジャンパ G15→G13 の実機検証と同一線)。
  TXD-3 はホスト→モジュール線を G13 (= LAN CS!) に繋いでしまい、受信だけ通って
  送信 (CNOK) が届かない症状になる。シルク16→G18 / シルク17→G17 が正。

### 実機疎通で確定した追加条件

1. **ボーレートは 9600bps 8N1** (タニタ FC-1200/ALBLO 仕様、fc1200-wasm README)。
   当初 firmware が M5 サンプル値 115200 のままで受信ゼロだった。
2. **DB9 の線序トグルスイッチ**がストレート/クロスを切り替える。位置が合わないと
   DIP・firmware が正しくても受信ゼロ。疎通しないときはまずここを反対へ。
3. **M-Bus 5V 出力 (AW9523 BUS_EN P0_1 + BOOST_EN P1_7)** を firmware で有効化
   しないとモジュール自体が無電源 (`hub-board/src/power.rs`)。
4. FC-1200 は RQCN 接続要求に **CNOK を返さないと測定フローへ進まない**
   (受信専用パススルーでは接続リトライで止まる)。プロトコルは
   `hub-core/src/fc1200.rs` (fc1200-wasm 移植) を参照。

## LAN Module 13.2 (W5500, PoE) — 未実装スタブ (`lan.rs`)

### CoreS3 デフォルトピン (公式サンプルで確認済み)

M5Stack 公式 `M5Module-LAN-13.2` の `examples/LinkStatus/LinkStatus.ino` は
`m5::board_t` で board を判定してピンを切り替える。CoreS3 分岐の値:

| 信号 | CoreS3 (単体) | 参考: Core (無印) | 参考: Core2 |
|---|---|---|---|
| CS | **G1** | G26 | G33 |
| RST | **G0** | G13 | G0 |
| INT | **G10** | G35 | G35 |

- SPI (SCK/MISO/MOSI) は board 判定で M5Unified が自動設定 (CoreS3 内蔵 SPI)。

### JC ジャンパ (差し替え式、3 組) — 実機確認済み

M-Bus 側に **3 組の JC ジャンパ (INT / RST / CS)**。各ジャンパは 3 ピン
(片側=選択肢A / 中央=信号 / 片側=選択肢B) で、シルク番号 (バスpin/無印 Core 基準) →
CoreS3 実 GPIO はスタック互換ツールで確定:

| 信号 | ジャンパ選択肢 (シルク) | デフォルト → CoreS3 | 変更後 → CoreS3 |
|---|---|---|---|
| INT | G35 / G34 | **G35 → G10** | G34 → G14 |
| RST | G0 / G13 | **G0 → G0** | G13 → G7 |
| CS | G5 / G15 | **G5 → G1** | **G15 → G13** |

- **出荷時デフォルト (キャップが G35 / G0 / G5)** = CoreS3 で INT=10 / RST=0 / CS=1
  → `LinkStatus.ino` の CoreS3 デフォルトと一致 (LAN 単体運用ならこのまま)。

### ★RS232M と同時スタック時: CS ジャンパを G15 へ

スタック互換ツールが示す唯一の競合:

- LAN の **CS デフォルト (G5 → CoreS3 G1)** が **RS232M の CS (CoreS3 G1、バス20)** と衝突。
- 回避: **CS ジャンパを G5 → G15 に動かす** (CoreS3 G13、バス23 に逃がす)。
- **INT (G35=G10) と RST (G0=G0) はデフォルトのまま**変更不要。
- firmware 側: LAN 実装時に **CS を 1 → 13** に変更する (`lan.rs`)。INT=10 / RST=0 は不変。

注: G13 は CoreS3 の I2S_DOUT。ハブが I2S スピーカーを使う場合は別途注意。

### 給電

- 本モジュールの **PoE (IEEE802.3at)** から給電する設計。

## スタック時の設定まとめ (RS232M + LAN + CoreS3)

| 対象 | 設定 |
|---|---|
| RS232 RXD DIP | **2 番 (16) ON** → G17 |
| RS232 TXD DIP | **3 番 (15) ON** → G18 |
| LAN INT ジャンパ | G35 のまま (=G10) |
| LAN RST ジャンパ | G0 のまま (=G0) |
| LAN CS ジャンパ | **G15 へ移動** (=G13、RS232 競合回避) |
| firmware `rs232.rs` | TX=G17 / RX=G18 (変更不要) |
| firmware `lan.rs` | 実装時に CS=13 / RST=0 / INT=10 |

## TODO

- [x] RS232M Module の DIP 実配置確認 (RXD-2=G17 / TXD-3=G18)
- [x] LAN Module JC ジャンパ ↔ CoreS3 GPIO 対応を確定 (INT G35=G10 / RST G0=G0 / CS G5=G1・G15=G13)
- [x] LAN Module 13.2 (W5500) リンク監視実装 (`lan.rs` → `eth_w5500.rs`、CS=13。
      LCD との SPI バス共有 + G35 二役対応は `hub-board/src/display.rs` 参照)
- [x] 実機で RS232 受信を確認 (FC-1200B の `RQCNFC-1200B` 受信、2026-07-15)
- [ ] 実機で LAN リンクアップと RS232 受信の同時動作を確認

## 第一段: LAN Module 取り外し構成 — NFC + スピーカー優先 (2026-07-21 決定)

NFC タップ連携と内蔵スピーカー (読み取りビープ) を早期に成立させるため、
**LAN Module 13.2 を取り外し WiFi 運用に戻す**。これにより:

| 機能 | ピン | 備考 |
|---|---|---|
| スピーカー (AW88298) | I2S DOUT=G13 / BCK=G34 / WS=G33 | LAN CS (G15→G13) が消えて**復活** |
| NFC (I2C1) | **Port A = SDA G2 / SCL G1** | LAN CS G5(=G1) 案も消えるので衝突なし。AtomS3 ベンチと同一ピン番号 |
| FC-1200 (RS232M) | G17/G18 | 無変更 (DIP もそのまま) |
| マイク (ES7210) | DIN=G14 | 衝突なしで温存 |
| ネットワーク | WiFi | lan.rs/eth_w5500 はコード温存 (モジュール非搭載では未初期化) |

有線 LAN / PoE が必要になった段階で下記の Base LAN PoE v1.2 構成へ移行する。

## 将来段: CoreS3 SE + Base LAN PoE v1.2 (2026-07-21 ピンマップ検討で確定)

上記までの一次情報は CoreS3 + **LAN Module 13.2** のスタック構成で、LAN CS ジャンパが
G5(=G1) / G15(=G13) の二択しかなく **内蔵スピーカー (I2S DOUT=G13 固定) と逃げ場なく
競合**していた (NFC 検証を AtomS3 Lite へ逃がした理由の一つ)。

**Base LAN PoE v1.2** は CS が **G9** に出ており、この競合が解消する。スタック互換
ツールのピンマップ (CoreS3 SE + Module13.2 RS232M + Base LAN PoE v1.2) で確認した割当:

| 機能 | ピン | 備考 |
|---|---|---|
| スピーカー (I2S) | DOUT=G13 / LRCK=G0 | **使用可**。RS232M ジャンパと LAN DB9 を避けること |
| LAN (W5500, PoE) | SPI G37/G35/G36 + CS=G9 + RST=G7 + INT=G14 | PoE 給電でケーブル1本運用 |
| FC-1200 (RS232M) | TX=G10 / RX=G6 | **現行 G17/G18 からジャンパ移動 + rs232.rs のピン変更** |
| NFC (I2C1) | **Port C = G17/G18** | RS232M 退去で空く。Port A は将来の I2C 周辺機器用に温存 |
| 犠牲 | マイク (G14=LAN INT)、内蔵バッテリー (SE) | 常設 PoE 給電機なら実害なし |

- **G13/G0 (スピーカー) を空ける条件**: RS232M の RX ジャンパを G13/G0 以外 (G6) に
  すること、Base LAN の DB9 (RS232/RS485、RX=G13 / TX=G1) を使わないこと
- **CoreS3 SE で削られるもの** (IMU/磁気/RTC/バッテリー) は hub 用途では未使用。
  バッテリーレスは充放電管理からの解放であり、時刻は起動時 NTP (SNTP) 同期で RTC レス運用
- 停電 = 即断となるため、点呼常設機としては PoE スイッチ側 UPS 等のインフラ側考慮が必要
- NFC の Port C 移行・rs232.rs のピン変更は `plan/nfc-card-identity.md` の
  「CoreS3 還元計画」セクション参照
