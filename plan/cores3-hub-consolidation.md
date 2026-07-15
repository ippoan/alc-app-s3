# CoreS3 統合ハブ — モジュール配線一次情報 (RS232M / LAN 13.2)

M5Stack CoreS3 に RS232M Module 13.2 と LAN Module 13.2 (W5500/PoE) をスタックして
ハブ化する際の GPIO 割当・ジャンパ/DIP 設定の一次情報。`hub-drivers/src/rs232.rs` /
`hub-drivers/src/lan.rs` のコメントが本ファイルを参照する。

一次情報:

- RS232 サンプル: https://github.com/m5stack/M5Stack/blob/master/examples/Modules/RS232/RS232.ino
- LAN ライブラリ: https://github.com/m5stack/M5Module-LAN-13.2
- LAN Module 公式 doc: https://docs.m5stack.com/en/module/LAN%20Module%2013.2
- 引継ぎ issue: ippoan/alc-app#102

## RS232M Module 13.2 → DB9 → FC-1200

UART1 パススルー (`rs232.rs` 実装済み)。

| 信号 | ピン |
|---|---|
| TX | G17 |
| RX | G18 |

- **設定手段は DIP スイッチ**。TX/RX に割り当てる GPIO を DIP で選ぶ。
- ⚠️ **シルク番号 ≠ GPIO 番号の実例あり** (M5 Community #5581)。DIP 刻印を鵜呑みにせず
  実機で導通確認する。上記 G17/G18 は想定値で、実機での DIP 実配置確認は未完了。
- **G13 / G0 / G14 は使用不可** — CoreS3 内蔵 I2S が占有済み。RS232M の DIP をこれらに
  振らないこと。

## LAN Module 13.2 (W5500, PoE) — 未実装スタブ (`lan.rs`)

### CoreS3 デフォルトピン (公式サンプルで確認済み)

M5Stack 公式 `M5Module-LAN-13.2` の `examples/LinkStatus/LinkStatus.ino` は
`m5::board_t` で board を判定してピンを切り替える。CoreS3 分岐の値:

| 信号 | CoreS3 | 参考: Core (無印) | 参考: Core2 |
|---|---|---|---|
| CS | **G1** | G26 | G33 |
| RST | **G0** | G13 | G0 |
| INT | **G10** | G35 | G35 |

- SPI (SCK/MISO/MOSI) は board 判定で M5Unified が自動設定 (CoreS3 内蔵 SPI)。
  `LinkStatus.ino` の board 分岐は CS/RST/INT のみを明示する。

### JC ジャンパ (差し替え式) — 場所と注意

- LAN Module 13.2 は **M5-Bus 側に 3 組の JC ジャンパキャップ** (CSN / RSTN / INTN) を
  持つ差し替え式 (**DIP ではない**、半田ジャンパでもない)。公式 module doc に記載。
- 公式 doc の切替値 (CSN: G26⇔G0 / RSTN: G13⇔G5 / INTN: G34⇔G35) は
  **無印 Core (ESP32) の GPIO 基準**。CoreS3 は上表のとおり GPIO 体系が異なり、
  **CoreS3 での「ジャンパ変更後」GPIO は公式サンプルに定義がない**。
- ⚠️ CoreS3 でジャンパを動かした場合の変更後 GPIO は **回路図 (`Sch_Module13.2_LAN.pdf`)
  で JC ピン → M5-Bus 物理ピンを追い、CoreS3 の M5-Bus→GPIO へ翻訳して確定する**必要が
  ある。実機未確認。

### G10 競合 (RS232M と併用時)

- LAN の INT デフォルト (G10) が RS232M 候補ピンと競合し得る。
- 回避: **RS232M の DIP を G10 に振らない**を第一とし、必要なら **LAN 側 INTN の JC
  ジャンパ**で INT を逃がす (逃がし先 GPIO は上記のとおり要確認)。

### 給電

- 本モジュールの **PoE (IEEE802.3at)** から給電する設計。

## TODO (実機確認待ち)

- [ ] RS232M Module の DIP スイッチ実配置確認 (G17/G18 想定)
- [ ] LAN Module JC ジャンパ差し替え時の CoreS3 GPIO を回路図/実機で確定
- [ ] LAN Module 13.2 (W5500) リンク監視・クラウド接続実装 (`lan.rs`)
