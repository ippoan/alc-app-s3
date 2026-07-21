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
  ├─ LCD (ILI9342C 320x240) + タッチ (FT5x06) ← 画面処理 (hub-ui)
  ├─ ネイティブ USB-C (USB Serial/JTAG)       ← ホストリンク + Improv (hub-drivers)
  ├─ M-Bus: RS232M Module → DB9 → FC-1200     ← UART1 パススルー (実装済み)
  ├─ 内蔵 BLE → NT-100B / NBP-1BLE            ← 実装済み (hub-ble,
  │                                             ble-medical-gateway 移植)
  ├─ 内蔵 Wi-Fi (2.4GHz)                       ← Improv 設定 + 自動再接続 (hub-wifi)
  └─ LAN Module 13.2 (W5500, PoE)             ← 未実装スタブ (hub-drivers/lan.rs)
```

想定フロー (Windows 排除案): タブレットで顔認証 → ホストが `QR <token>` を送信 →
CoreS3 画面に QR 表示 → 読み取り → `MEASURE` → FC-1200 で測定 → `RESULT OK 0.000`。

## 画面遷移 (タッチ主導のキオスクフロー)

```
           ┌─(上半分タップ)→ Measuring(点呼) ─(RESULT cmd)→ Result ─┐
Idle ─タップ→ Menu                                         自動/タップ│
(NFC待機)  └─(下半分タップ)→ Log(ログ確認) ─タップ→ Idle             │
  ↑  ↑                                                             │
  │  └─────────────────────────────────────────────────────────────┘
  ├─ BLE 測定受信 (待機中/点呼中のみ・QR等の操作中は遷移しない) → 体温/血圧 表示 ─タップ/30秒→ Idle
  └─ ホストコマンド: QR / MEASURE / RESULT / ERROR / RESET は従来どおり
```

- 基準文字サイズは 16px フォントの 2 倍拡大描画 (実効 32px)。数値は Logisoso42
- 全画面上部にステータスバー (LAN / 232 / BLE / WiFi + 稼働時間、18px・小サイズ)。
  毎秒の時計更新は背景色付きテキストの上書きのみで blink しない

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
| `STATUS` | `STATUS LAN=0 RS232=1 BLE=0 WIFI=0 ROT=0` 応答 |
| `CFG GET` | 現在の設定を 1 行 JSON でエクスポート |
| `CFG SET <json>` | 設定 (画面向き + Wi-Fi) を検証して NVS へインポート |
| `WIFI TEST` | 保存済み Wi-Fi 設定で接続テスト (失敗時は原因を切り分け) |
| `PAIR` / `BLE PAIR` | BLE の全ボンド消去 → 次接続で再ペアリング |
| `AUTH SET <id> <secret> <tenant>` | device credential を注入 (USB provisioning。ホストが auth-worker `/device/pair` 系で取得した値) |
| `AUTH UNPAIR` / `AUTH STATUS` | credential の破棄 / 状態確認 (`AUTH PAIRED <tenant> <id>` or `AUTH UNPAIRED`) |
| `AUTH TOKEN` | device JWT 取得の自己診断 (`EVT AUTH_TOKEN OK\|NG ...`) |
| `AUTH URL <url>` / `WS URL <url>` | auth-worker / cf-alc-recorder の URL 上書き (staging テスト用、NVS 保存) |
| `WS STATUS` | `WS CONNECTED=1 QUEUE=3 SEQ=42` 応答 (測定データ WS 送信の状態) |
| `GW URL <ws://...>` | Windows GW (alc-gw) ハブ URL の手動オーバーライド (NVS)。**通常は不要** — GW の UDP beacon (9001) を自動発見して接続する。WS 下り command `{action:"gw_url",url}` / `{action:"gw_status"}` (auth-worker /device/setup) でも遠隔で設定・確認できる |
| `GW STATUS` | `GW CONNECTED=1 URL=UNSET DISCOVERED=ws://192.168.11.5:9000` 応答 |

同一ストリームで **Improv Wi-Fi Serial** のバイナリフレームも受け付ける
(ESP Web Tools / Pages の Wi-Fi 設定用。crates/hub-wifi/src/improv.rs /
crates/hub-core/src/improv.rs)。

| CoreS3 → ホスト | 説明 |
|---|---|
| `FC1200 <hex>` | RS232 (FC-1200) 受信データのパススルー |
| `EVT QR_TIMEOUT` / `EVT RESULT_CLOSED` | 画面の自動遷移通知 |
| `EVT TENKO_START` | 画面メニューから点呼が開始された |
| `EVT WIFI_TEST OK\|NG <詳細>` | `WIFI TEST` の結果 (NG は原因を切り分け) |
| `EVT PAIR_CLEARED` | BLE ボンド消去完了 |
| `EVT WS_CONNECTED` / `EVT WS_DISCONNECTED` | cf-alc-recorder への WS 接続状態 |
| `EVT GW_CONNECTED` / `EVT GW_DISCONNECTED` | Windows GW (alc-gw) への WS 接続状態 |
| `EVT WS_COMMAND <id> <payload>` | サーバからの下り command (MEASURE 指示 / timecard 等) |
| `EVT WS_DROPPED <seq>` | 送信キュー上限で最古の未送信測定を破棄 |
| `EVT CRASH <reason> log_bytes=<n>` | 前回リセットがクラッシュ由来 (panic/WDT/brownout 等)。panic 前ログを kind=crash_log で自動送信 |
| `CFG <json>` | `CFG GET` の応答 |
| `{"type":"temperature",...}` 等 | BLE 測定データ・状態。[ble-medical-gateway](https://github.com/ippoan/ble-medical-gateway) のシリアル JSON 互換 (alc-app 側 `useBleGateway` を流用可能) |

ESP-IDF のログが同じコンソールに混在するため、ホスト側は既知プレフィックス
(`OK` `ERR` `PONG` `STATUS` `FC1200` `EVT` `CFG` `{`) の行のみ解釈すること。

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
  — **PR = coverage 100% チェック + xtensa `cargo check`** (main の warm
  キャッシュを restore 専用で利用、`ippoan/ci-workflows` の reusable
  auto-merge で自動マージ)、**main = フルビルド + イメージ生成 + Pages
  デプロイ + キャッシュ warm (save は main のみ)**
- 書き込みイメージ: `espflash save-image --merge` によるオフセット 0 の単一 bin
  ([partitions.csv](partitions.csv): factory 8MB / 16MB flash)
- **画面向き設定**: インストールページ上の「画面向き設定」から Web Serial 経由で
  `ROTATE` コマンドを送信して設定 (0/90/180/270°、NVS 保存)。設置向きに合わせて
  書き込み直後にブラウザだけで完結する

## クレート構成 (再コンパイル範囲の最小化 + 並列ビルド)

```
hub-core (純粋) → hub-common (状態/設定/UIコマンド/測定値/制御フラグ)
                    ├→ hub-ble   (体温計/血圧計)          ┐
                    ├→ hub-wifi  (Wi-Fi + Improv)         ├ 互いに独立 = 並列ビルド
                    ├→ hub-drivers (ホストリンク/RS232/    ┘ (drivers は wifi にも依存)
                    │              NTP/recorder)
                    └→ hub-ui    (画面。hub-board にも依存)
hub-board (ボード初期化, 独立葉)   ルート = main の配線のみ
```

| クレート | 内容 | 変更頻度 |
|---|---|---|
| [crates/hub-core](crates/hub-core) | 純粋ロジック (ホストでテスト・coverage 100%): IEEE 11073 デコード / プロトコル解析 / 設定 JSON / 時刻整形 / コエグジスト調停 / レイアウト | 低 |
| [crates/hub-common](crates/hub-common) | 共有基盤 (状態 / NVS 設定 / 測定値型 / 制御フラグ / UI コマンド) | 低 |
| [crates/hub-board](crates/hub-board) | CoreS3 ボード初期化 (LCD / 電源 / タッチ) | 低 |
| [crates/hub-ble](crates/hub-ble) | BLE central (NT-100B / NBP-1BLE) | 低 |
| [crates/hub-wifi](crates/hub-wifi) | Wi-Fi STA (自動再接続) + Improv Wi-Fi Serial | 低 |
| [crates/hub-drivers](crates/hub-drivers) | ホストリンク / RS232 / NTP / recorder / LAN スタブ | 低 |
| [crates/hub-ui](crates/hub-ui) | 画面処理 (状態機械 + 描画) | **高 (画面遷移の変更はここだけ)** |

CI ではワークスペース内クレートが checkout の mtime 変化で毎回再コンパイル
されるため、内容ベースの **sccache** (GHA バックエンド) で吸収している
(rust-alc-api と同方式)。xtensa クロスビルド (esp-idf-sys/embuild) は Cargo に
深く結合しており、rust-alc-api のような Bazel 化はホスト側テスト以外では
割に合わないと判断 — Bazel の利点 (内容ベースキャッシュ) は sccache で取る。

## テスト / カバレッジ 100% (ippoan/rust-alc-api と同方式)

ESP-IDF に依存しない純粋ロジック (IEEE 11073 デコード・ホストプロトコル解析・
デバイス名判定・レイアウト計算) は [crates/hub-core](crates/hub-core) に分離し、
ホスト上で単体テストする。[coverage_100.toml](coverage_100.toml) に登録された
ファイルは PR CI (`cargo llvm-cov` +
[scripts/check_coverage_100.sh](scripts/check_coverage_100.sh)) で
**ラインカバレッジ 100%** が強制される。

```powershell
# ホストでのテスト実行 (esp ツールチェーン不要)
$env:RUSTUP_TOOLCHAIN='stable'; cargo test -p alc-hub-core --target x86_64-pc-windows-msvc
```

新しい純粋ロジックは hub-core に追加し、coverage_100.toml へ登録すること。

## ビルド

Rust の ESP32 (Xtensa) ツールチェーンが必要:

```powershell
cargo install espup ldproxy espflash
espup install --targets esp32s3   # 'esp' ツールチェーンを導入
cargo build --release             # 初回は ESP-IDF v5.5.3 を自動取得 (時間がかかる)
```

### ローカル書き込み (Windows)

編集 → ビルド → 実機書き込みは [local/flash.ps1](local/README.md) で一発:

```powershell
.\local\flash.ps1            # ビルド + COM4 の CoreS3 へ書き込み
.\local\flash.ps1 -Monitor   # 書き込み後にシリアルモニタも開く
```

ESP-IDF は出力パスが長いと "Too long output directory" で失敗するため、
`flash.ps1` は `CARGO_TARGET_DIR=C:\t\alcs3` (短いパス) と
`ESP_IDF_SDKCONFIG_DEFAULTS` の絶対指定でこれを回避している (詳細は
[local/README.md](local/README.md))。CoreS3 は USB Serial/JTAG のため、Pages の
タブや他のシリアルモニタが COM を掴んでいると書き込みに失敗する (先に閉じる)。

動作確認 (シリアルモニタから):

```
PING
QR https://example.com/tenko/abc123 30
MEASURE
RESULT OK 0.000
STATUS
```

## BLE (NT-100B / NBP-1BLE)

`ippoan/ble-medical-gateway` の移植 (crates/hub-ble):

- esp32-nimble 0.12 / ESP-IDF v5.5.3 (firmware-rust PoC とバージョン一致)
- 連続スキャン → 発見次第接続 (最大 3 リトライ) → indication/notification 購読
  → IEEE 11073 FLOAT/SFLOAT デコード → JSON 出力
- 対象判定: 標準サービス UUID (0x1809 / 0x1810) + デバイス名 (NT-100 / NBP-1 等)
- 測定値は画面に大きく表示 (体温/血圧画面) + イベントログ + シリアル JSON

**実機で判明した挙動と対策 (2026-07-12、実機ログで確認・修正):**

- **notify コールバックの軽量化**: 受信 (nimble_host タスク・スタック小) では
  パースしてバッファに積むだけにし、JSON 出力 / NVS 記録 / 画面通知は専用の
  `recorder` スレッドで行う。以前はコールバックで直接やって血圧受信時に
  スタックオーバーフロー→再起動していた
- **過去分ダンプ対策**: 血圧計は標準サービスの仕様で保存済みの過去測定を
  まとめて送る。セッション中の測定を貯め、機器タイムスタンプが**最新の 1 件**
  だけを記録する (ieee11073 でタイムスタンプを解析)
- **データ待ちタイムアウト (5 秒)**: 接続したがデータが来ない機器に張り付くと
  BLE の supervision timeout (~99 秒) までループ全体がブロックされ、体温も血圧も
  取れなくなる。5 秒で切断して再スキャンへ戻す
- **コエグジスト**: Wi-Fi 接続/スキャン中は BLE スキャンを一時停止 (RadioCoex)。
  `CONFIG_ESP_COEX_SW_COEXIST_ENABLE=y`
- **NimBLE ホストタスクのスタック拡張**: `CONFIG_BT_NIMBLE_HOST_TASK_STACK_SIZE=8192`

## Wi-Fi (Improv Wi-Fi Serial) + NTP + 測定ログ永続化

**Wi-Fi 設定** — 2 通り (どちらも NVS 保存され起動時に自動接続):

1. **Pages の「Wi-Fi 設定 (いつでも)」フォーム** — ページの JS が Web Serial で
   Improv プロトコルを直接話す。説明文を読みながらいつでも設定できる
2. ESP Web Tools のインストールダイアログ (書き込み直後の「Wi-Fi 設定」)

2.4GHz (11b/g/n) のみ・WPA/WPA2/WPA3-Personal 対応。主経路はあくまで
LAN Module 13.2 (PoE) で、Wi-Fi は LAN 配線が無い拠点向けの代替経路。

- **自動再接続 (keepalive)**: 切断を検出したら再接続。失敗が続く場合は段階的に
  バックオフ (15 秒→最大 5 分)、接続タイムアウトも短め (8 秒) にして、単一
  2.4GHz 無線を共有する BLE (医療機器・優先) への妨害を最小化する
- **接続テスト**: `WIFI TEST` / Pages のボタンで保存済み設定を試し、失敗時は
  その場でスキャンして原因を切り分け (SSID 不可視=2.4GHz/SSID 違い、可視=
  パスワード/認証違い)

**NTP** — ネットワーク接続後に SNTP で時刻同期し、ログを日本時間で記録
(crates/hub-drivers/src/ntp.rs)。同期前は稼働時間にフォールバック
(時刻整形は hub-core::clock、テスト付き)。

**測定ログの永続化** — 測定値と接続イベントを NVS に保存 (直近 20 件)。
リブートしても消えず、起動時に「ログ確認」画面へ復元する。時刻ラベルは
NTP 同期済みなら `MM/DD HH:MM:SS` (JST)、未同期なら稼働時間で統一。

## 音声フィードバック (クレジット表記)

NFC 読み取り時の音声「登録完了しました」は **VOICEVOX:四国めたん**
(https://voicevox.hiroshiba.jp/) で生成した合成音声を使用している
(`crates/hub-drivers/assets/touroku_kanryo_24k_s16le.raw`、24kHz mono s16le —
VOICEVOX 内部ネイティブレートのまま無加工で持ち、再生時に ×2 線形補間)。
VOICEVOX の利用規約によりクレジット表記が必要 — 本製品を紹介する資料や
配布物にも「VOICEVOX:四国めたん」を記載すること。
音源の再生成手順: VOICEVOX ENGINE の `/audio_query` → `/synthesis` API で
`outputSamplingRate=24000` / `outputStereo=false` を指定し、無音トリムのみで
差し替える (正規化やローパスは掛けない — エンジン側 48kHz リサンプルの
シャリつき・増幅によるノイズ床上昇を実機で確認済み。issue #101/#102)。

## 設定インポート/エクスポート

画面向きと Wi-Fi 設定を JSON で一括バックアップ/復元できる
(`CFG GET` / `CFG SET`、Pages の「設定のエクスポート/インポート」カード)。
複数台への同一設定配布や、接続不良の切り分けに使う。ローカルからは
[local/device-config.json](local/README.md) を `CFG SET` に流し込む。

## TODO

- [ ] 実機での LCD 初期化確認 (色順 `ColorOrder` / 回転は要調整の可能性)
- [ ] 90/270 回転時のタッチ座標変換の実機確認 (layout::map_touch)
- [ ] RS232M Module の DIP スイッチ実配置確認 (G17/G18 想定)
- [ ] BLE と Wi-Fi 同時使用 (コエグジスト) 時のメモリ・安定性の実機確認
- [ ] LAN Module 13.2 (W5500) リンク監視・クラウド接続 (src/lan.rs)
- [ ] FC-1200 プロトコル解釈: `fc1200-wasm` の UART 直結移植 (現状は hex パススルー)
- [ ] NFC (Unit NFC / ST25R3916 CE モード) — alc-app#100 の調査メモ参照、当面スコープ外。
      読み取れたら gw_link の `nfc_read` (alc-gw README 参照) で GW へ送る
- [x] Windows GW (alc-gw) 連携 — 測定の生中継 + 下り測定開始 (`GW URL` で有効化、gw_link.rs)
