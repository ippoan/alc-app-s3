# alc-app-s3 — M5Stack CoreS3 統合ハブ (画面処理)

`ippoan/alc-app` の点呼キオスクを CoreS3 (ESP32-S3) に統合する計画
([plan/cores3-hub-consolidation.md](https://github.com/ippoan/alc-app/blob/main/plan/cores3-hub-consolidation.md))
に基づく、**Rust (ESP-IDF)** 製ファームウェア。本リポジトリはまず**画面処理**
(待機 / QR 表示 / 測定中 / 結果 / エラー / 機器ステータス) を実装する。

関連 issue: [alc-app#100](https://github.com/ippoan/alc-app/issues/100) (NFC 調査メモ),
[alc-app#102](https://github.com/ippoan/alc-app/issues/102) (RS232 サンプル・LAN Module 一次情報)

## 構成

```
CoreS3
  ├─ LCD (ILI9342C 320x240) + タッチ (FT5x06) ← 本リポジトリの画面処理
  ├─ ネイティブ USB-C (USB Serial/JTAG)       ← ホストリンク (行指向プロトコル)
  ├─ M-Bus: RS232M Module → DB9 → FC-1200     ← UART1 パススルー (実装済み)
  ├─ 内蔵 BLE → NT-100B / NBP-1BLE            ← 実装済み (src/ble.rs,
  │                                             ble-medical-gateway 移植)
  └─ LAN Module 13.2 (W5500, PoE)             ← 未実装スタブ (src/lan.rs)
```

想定フロー (Windows 排除案): タブレットで顔認証 → ホストが `QR <token>` を送信 →
CoreS3 画面に QR 表示 → 読み取り → `MEASURE` → FC-1200 で測定 → `RESULT OK 0.000`。

## 画面遷移

```
Boot ─→ Idle ─(QR)─→ Qr ─(MEASURE)─→ Measuring ─(RESULT)─→ Result ─┐
         ↑            │timeout → EVT QR_TIMEOUT          自動/タップ │
         ├────────────┴─────────────────────────────────────────────┘
         ├─(タップ)─→ StatusDetail ─(タップ)─→ Idle
         └─(ERROR はどの画面からでも)─→ Error ─(タップ/RESET)─→ Idle
```

全画面上部にステータスバー (LAN / 232 / BLE インジケータ + 稼働時間)。

## ホストプロトコル (USB CDC, 行指向)

| ホスト → CoreS3 | 説明 |
|---|---|
| `PING` | 疎通確認 (`PONG` 応答) |
| `QR <payload> [timeout_s]` | QR コード表示 (既定 60 秒で期限切れ) |
| `MEASURE` | 測定中画面 |
| `RESULT OK\|NG [value]` | 結果画面 (10 秒で自動クローズ) |
| `ERROR <message>` | エラー画面 |
| `RESET` | 待機画面へ |
| `ROTATE <0\|90\|180\|270>` | 画面向き変更 (NVS 保存、再起動後も維持) |
| `STATUS` | `STATUS LAN=0 RS232=1 BLE=0 ROT=0` 応答 |

| CoreS3 → ホスト | 説明 |
|---|---|
| `FC1200 <hex>` | RS232 (FC-1200) 受信データのパススルー |
| `EVT QR_TIMEOUT` / `EVT RESULT_CLOSED` | 画面の自動遷移通知 |
| `{"type":"temperature",...}` 等 | BLE 測定データ・状態。[ble-medical-gateway](https://github.com/ippoan/ble-medical-gateway) のシリアル JSON 互換 (alc-app 側 `useBleGateway` を流用可能) |

ESP-IDF のログが同じコンソールに混在するため、ホスト側は既知プレフィックス
(`OK` `ERR` `PONG` `STATUS` `FC1200` `EVT`) の行のみ解釈すること。

## ピン割当 (机上調査ベース・実機未検証)

| 用途 | ピン | 備考 |
|---|---|---|
| LCD SPI2 | SCLK=G36 / MOSI=G37 / CS=G3 / DC=G35 | M5GFX CoreS3 定義準拠。RST=AW9523 P1_1, BL=AXP2101 DLDO1 |
| タッチ I2C | SDA=G12 / SCL=G11 (0x38) | AXP2101(0x34) / AW9523(0x58) と共用 |
| RS232M | TX=G17 / RX=G18 | DIP スイッチ候補。**シルク番号≠GPIO 番号の実例あり (Community #5581)、実機で要確認** |
| LAN Module | CS=G1 / RST=G0 / INT=G10 | 未実装。G10 は RS232M 候補と競合し得る (ジャンパで回避可) |

G13 / G0 / G14 は CoreS3 内蔵 I2S が使用済みのため RS232M では使用不可。

## リリース (GitHub Pages)

main への push で GitHub Actions がファームウェアをビルドし、
**https://ippoan.github.io/alc-app-s3/** に ESP Web Tools の書き込みページを
デプロイする (ble-medical-gateway と同方式)。CoreS3 を USB-C で接続し、
Chrome/Edge からブラウザだけで書き込める。

- ワークフロー: [.github/workflows/build.yml](.github/workflows/build.yml)
  (PR は `ippoan/ci-workflows` の reusable auto-merge で自動マージ。
  キャッシュは main への push で warm され、PR ビルドが参照する)
- 書き込みイメージ: `espflash save-image --merge` によるオフセット 0 の単一 bin
  ([partitions.csv](partitions.csv): factory 8MB / 16MB flash)
- **画面向き設定**: インストールページ上の「画面向き設定」から Web Serial 経由で
  `ROTATE` コマンドを送信して設定 (0/90/180/270°、NVS 保存)。設置向きに合わせて
  書き込み直後にブラウザだけで完結する

## ビルド

Rust の ESP32 (Xtensa) ツールチェーンが必要:

```powershell
cargo install espup ldproxy espflash
espup install            # 'esp' ツールチェーンを導入
cargo build --release    # 初回は ESP-IDF v5.5.3 を自動取得 (時間がかかる)
cargo run --release      # espflash flash --monitor
```

動作確認 (シリアルモニタから):

```
PING
QR https://example.com/tenko/abc123 30
MEASURE
RESULT OK 0.000
STATUS
```

## BLE (NT-100B / NBP-1BLE)

`ippoan/ble-medical-gateway` の移植 (src/ble.rs):

- esp32-nimble 0.12 / ESP-IDF v5.5.3 (firmware-rust PoC とバージョン一致)
- 3 秒スキャン → 発見次第接続 (最大 3 リトライ) → indication/notification 購読
  → IEEE 11073 FLOAT/SFLOAT デコード → JSON 出力 → 2 秒後に切断・再スキャン
- Just Works ボンディング (AuthReq::Bond / NoInputNoOutput)
- 対象判定: 標準サービス UUID (0x1809 / 0x1810) + デバイス名 (NT-100 / NBP-1 等)

## TODO

- [ ] 実機での LCD 初期化確認 (色順 `ColorOrder` / 回転は要調整の可能性)
- [ ] RS232M Module の DIP スイッチ実配置確認 (G17/G18 想定)
- [ ] BLE と Wi-Fi/画面同時使用時の実機動作確認 (NimBLE のメモリ・GPIO 競合)
- [ ] LAN Module 13.2 (W5500) リンク監視・クラウド接続 (src/lan.rs)
- [ ] FC-1200 プロトコル解釈: `fc1200-wasm` の UART 直結移植 (現状は hex パススルー)
- [ ] NFC (Unit NFC / ST25R3916 CE モード) — alc-app#100 の調査メモ参照、当面スコープ外
