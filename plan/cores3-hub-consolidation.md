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

### DIP 設定 (実機確認済み)

モジュールには **RXD 用 / TXD 用の 2 つの DIP (各 5 スイッチ)** があり、各スイッチの
シルク番号は **バスピン番号**。スタック互換ツールの CoreS3 翻訳と合わせると:

| DIP | ON にするスイッチ | シルク (バスpin) | CoreS3 GPIO | 意味 |
|---|---|---|---|---|
| **RXD** | **2 番** | 16 | **G17** (PC_TX) | ホスト TX → モジュール RX |
| **TXD** | **3 番** | 15 | **G18** (PC_RX) | モジュール TX → ホスト RX |

- RXD シルク並び: `3 / 16 / 13 / 34 / 35` (スイッチ 1〜5) → **2 番 (16)** を ON
- TXD シルク並び: `1 / 17 / 15 / 12 / 0` (スイッチ 1〜5) → **3 番 (15)** を ON
- **他のスイッチは全 OFF**。
- これで `rs232.rs` の TX=G17 / RX=G18 と一致する (**ファーム変更不要**)。
- `ON` 印字のある側へスライダを倒すと ON。

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
- [ ] LAN Module 13.2 (W5500) リンク監視・クラウド接続実装 (`lan.rs`、CS=13 で実装)
- [ ] 実機で RS232 受信 (`FC1200 <hex>`) と (LAN 実装後) リンクアップの同時動作を確認
