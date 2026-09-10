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
  └─ Base LAN PoE v1.2 (W5500, PoE)           ← `lan` feature (hub-drivers/lan.rs)
```

想定フロー (Windows 排除案): タブレットで顔認証 → ホストが `QR <token>` を送信 →
CoreS3 画面に QR 表示 → 読み取り → `MEASURE` → FC-1200 で測定 → `RESULT OK 0.000`。

## 画面遷移 (タッチ主導のキオスクフロー)

```
           ┌─(上半分タップ)→ Measuring(点呼) ─(RESULT cmd)→ Result ─┐
Idle ─タップ→ Menu                                         自動/タップ│
(NFC待機)  └─(下半分タップ)→ Log(ログ確認) ─タップ→ Idle             │
  ↑  ↑ │                                                           │
  │  │ └─免許証タップ→ Confirm(点呼確認) ─(上: 点呼を開始)→ Measuring │
  │  │        (下: キャンセル / 15秒放置 → Idle)                    │
  │  └─────────────────────────────────────────────────────────────┘
  ├─ BLE 測定受信 (待機中/点呼中のみ・QR等の操作中は遷移しない) → 体温/血圧 表示 ─タップ/30秒→ Idle
  └─ ホストコマンド: QR / MEASURE / RESULT / ERROR / RESET は従来どおり
```

- **免許証 (IC、NFC-B) をかざす**とメニューを飛ばして点呼確認画面へ直行する
  (点呼中・QR 表示中は奪わない)。ヘッダに交付日・有効期限を表示し、NTP 同期済みで
  期限切れなら赤字 (`EVT LICENSE_EXPIRED`)。「点呼を開始」の下に残り秒数 (あと N秒) を
  出し、15 秒放置で待機画面へ戻る。**免許証から始めた点呼では、点呼開始時に
  `kind=license` (`{type, nfc_id: 交付日8桁+有効期限8桁, issue, expiry}`) を測定と同じ
  `session_id` で WS へ送る** — サーバは `nfc_id` で乗務員 (`employees.nfc_id`) に結合する
  (alc-app タブレットと同じキー、Refs #125)
- かざしてから **15 秒間はメニューの「ログ確認」を押せない** (ボタンは残り秒数付きで
  グレー表示)。確認画面の「キャンセル」や待機画面の下半分を続けて叩いたときに
  ログ画面へ流れ込む誤操作を防ぐ (`crates/hub-core/src/tenko_prompt.rs`)

- **点呼画面は体温 + アルコールの 2 段が基本**。血圧は運用オプション (`TENKO BP ON`) で、
  ON のときだけ 3 段目に出て完了条件にも入る。OFF のときは血圧計の測定を画面では無視する
  (ログ・WS 送信には残る)。必須項目が揃うと 5 秒後に待機画面へ戻る
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
| `STATUS` | `STATUS LAN=0 RS232=1 BLE=0 WIFI=0 ROT=0 BOARD=cores3` 応答 (`BOARD` は起動時の I2C probe で `cores3` / `cores3se`) |
| `LOG DUMP` | 直近ログのリング (CoreS3 は PSRAM の `.ext_ram_noinit` に 256 KB、それ以外の機種は `.noinit` に 4 KB、#217) を `LOGDUMP ...` で吸い出す。事象の後から原因を追う用。WS 下り command `{action:"get_log",max_bytes?,offset?}` (`max_bytes` は省略時 3000・上限 3800、`offset` は末尾から遡るバイト数で省略時 0 = 末尾) でも同じリングの窓を行境界で切って `command_result` `{text,bytes,total_bytes,truncated,offset,uptime_ms,pwa_log,pwa_log_error}` で遠隔から取れる (auth-worker MCP `get_device_log`)。1 回で取り切れないときは `offset + bytes` を次の `offset` にして遡る (`offset` は実際に使った値。`total_bytes` に達したら終わり)。reset 履歴を持つ機種 (CoreS3) は直近 8 回の `boot_history` `[{reset_reason,reset_code}]` (新しい順) も足す。リングは電源断以外の reset をまたいで残り、起動ごとに `--- BOOT reset=<name> (<code>) ---` が入る。CoreS3 に USB で運行者 PWA が繋がっていれば、PWA のシリアル診断ログ (`PWALOG`、最大 2 秒待ち・末尾 1200 バイト) を `pwa_log` に足す (`pwa_log_error`: `null` / `"timeout"` / `"no_host"`、#215) |
| `CFG GET` | 現在の設定を 1 行 JSON でエクスポート |
| `CFG SET <json>` | 設定 (画面向き + Wi-Fi) を検証して NVS へインポート |
| `WIFI TEST` | 保存済み Wi-Fi 設定で接続テスト (失敗時は原因を切り分け) |
| `PAIR` / `BLE PAIR` | BLE の全ボンド消去 → 次接続で再ペアリング |
| `AUTH SET <id> <secret> <tenant>` | device credential を注入 (USB provisioning。ホストが auth-worker `/device/pair` 系で取得した値) |
| `AUTH UNPAIR` / `AUTH STATUS` | credential の破棄 / 状態確認 (`AUTH PAIRED <tenant> <id>` or `AUTH UNPAIRED`) |
| `AUTH TOKEN` | device JWT 取得の自己診断 (`EVT AUTH_TOKEN OK\|NG ...`) |
| `AUTH TICKET` | 端末登録の一回券を auth-worker から取得 (`AUTH TICKET <ticket> EXPIRES=<秒>` / `ERR AUTH TICKET: <理由>`)。運行者 PWA の端末登録用、**CoreS3 のみ**対応 |
| `AUTH KEYGEN [FORCE]` / `AUTH PUBKEY` | 警告デバイス (VoiceS3R) 管理者認証用の ed25519 鍵対を機体内で生成 (`AUTH PUBKEY <base64url>` を返す。既に在れば `ERR AUTH: key exists`、`FORCE` で作り直し) / 生成済み公開鍵の再提示 (無ければ `ERR AUTH: no key`)。秘密鍵は NVS のみに留まり USB には出ない (Refs #205) |
| `AUTH SIGN <nonce>` | サーバの nonce (小文字 hex 32 文字の ASCII、その 32 バイトそのものに署名) に署名し `AUTH SIG <pubkey base64url> <sig base64url>` を返す (鍵無しは `ERR AUTH: no key`、nonce の形式不正は `ERR AUTH: bad nonce`) |
| `AUTH URL <url>` / `WS URL <url>` | auth-worker / cf-alc-recorder の URL 上書き (staging テスト用、NVS 保存) |
| `WS STATUS` | `WS CONNECTED=1 QUEUE=3 SEQ=42` 応答 (測定データ WS 送信の状態) |
| `TENKO BP ON\|OFF` / `TENKO STATUS` | 点呼に血圧を含めるか (NVS 保存、**既定 OFF** = 体温 + アルコールの 2 段)。`TENKO BP=0` 応答 |
| `BUS5V STATUS` | M-Bus 5V 出力の現況 `BUS5V USB=1 OUT=1 BATTERY=0 BUS_IN=0` を返す。**設定は無い** (#202) — **USB ホスト (PC) が列挙されていて、かつ M-Bus が外部給電でない (`BUS_IN=0`) 間だけ** Core が M-Bus へ 5V を出す固定動作で、hub-ui の i2c ループが 1 秒ごとに追随する (起動時は出さない / 起動から 3 秒は読まない / 同じ値が 2 回続いてから切り替える)。`BUS_IN` は起動時の W5500 probe で確定し起動中は変わらない (`1`=PoE 等で外部給電中 `0`=無し `?`=未判定、Refs #211) — `BUS_IN` が `1`/`?` の間は切り替え自体を起こさない。切り替え時に `EVT BUS5V OUT=<0\|1> usb_host=<0\|1>` を出す。WS 下り command `{action:"bus5v_status"}` (応答 `{usb_host,ext_5v_out,battery_present,power_read,bus_in}`) / `{action:"reboot"}` (OTA 中・点呼中は `{ok:false,message:"busy"}`) で、auth-worker の端末一覧から遠隔で照会・再起動できる |
| `GW URL <ws://...>` | Windows GW (alc-gw) ハブ URL の手動オーバーライド (NVS)。**通常は不要** — GW の UDP beacon (9001) を自動発見して接続する。WS 下り command `{action:"gw_url",url}` / `{action:"gw_status"}` (auth-worker /device/setup) でも遠隔で設定・確認できる |
| `GW STATUS` | `GW CONNECTED=1 URL=UNSET DISCOVERED=ws://192.168.11.5:9000` 応答 |

同一ストリームで **Improv Wi-Fi Serial** のバイナリフレームも受け付ける
(ESP Web Tools / Pages の Wi-Fi 設定用。crates/hub-wifi/src/improv.rs /
crates/hub-core/src/improv.rs)。

| CoreS3 → ホスト | 説明 |
|---|---|
| `FC1200 <hex>` | RS232 (FC-1200) 受信データのパススルー |
| `EVT QR_TIMEOUT` / `EVT RESULT_CLOSED` | 画面の自動遷移通知 |
| `EVT TENKO_START` | 画面 (メニュー or 免許証タップ後の確認画面) から点呼が開始された |
| `EVT NFC_LICENSE issue=YYYYMMDD expiry=YYYYMMDD` | 運転免許証 (IC) を読み取った |
| `EVT LICENSE_EXPIRED <YYYYMMDD>` | 読み取った免許証が期限切れ (NTP 同期済みのときのみ判定) |
| `EVT TENKO_CANCEL` / `EVT CONFIRM_TIMEOUT` | 点呼確認画面をキャンセル / 15 秒放置で待機へ戻った |
| `EVT TENKO_SESSION <id>` | 点呼セッション ID を発番した (この点呼で採れた測定に載る、Refs #112) |
| `EVT WIFI_TEST OK\|NG <詳細>` | `WIFI TEST` の結果 (NG は原因を切り分け) |
| `EVT PAIR_CLEARED` | BLE ボンド消去完了 |
| `EVT WS_CONNECTED` / `EVT WS_DISCONNECTED` | cf-alc-recorder への WS 接続状態 |
| `EVT GW_CONNECTED` / `EVT GW_DISCONNECTED` | Windows GW (alc-gw) への WS 接続状態 |
| `EVT WS_COMMAND <id> <payload>` | サーバからの下り command (MEASURE 指示 / timecard 等) |
| `EVT WS_DROPPED <seq> <kind>` | 送信キューの保存先が一杯で最古の未送信測定を破棄 (kind = 測定種別。打刻は `timecard`) |
| `EVT PUNCHQ <mode> count=<n>` | 送信キューの保存先 (`nvs` = 専用パーティション punchq / `legacy` = 既定 nvs にフォールバック) と未送信件数 |
| `EVT PUNCHQ migrated <n>` | 旧形式 (既定 nvs の文字列) に残っていた n 件を punchq へ移した |
| `EVT WS_CLOCK_WAIT` | WS は繋がったが時計が未同期で、補正できる測定があるため送信を最大 60 秒待っている (NTP 同期後に補正して送る) |
| `EVT WS_TIME_FIXED <n>` | NTP 未同期 (ネットワーク無し) で記録した測定 n 件の `recorded_at_ms` を、送信時に稼働時間の差で実時刻へ補正した (同じ起動の分だけ。再起動をまたいだ分は 1970 起点のまま送る) |
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
| Base LAN PoE v1.2 | CS=G9 / RST=G7 / INT=G14 | `lan` feature。INT は未使用 (polling)。本体 DB9 (RX=G13 / TX=G1) は使わない |
| Unit NFC (I2C1) | SDA=G2 / SCL=G1 (Port A) | `nfc-verify` feature (既定 on)。ack しなければ SDA/SCL 入替 |

### CoreS3 SE への移行準備

CoreS3 SE は CoreS3 から カメラ / IMU / 地磁気 / RTC / 近接センサ / **内蔵バッテリー**
を削った板で、firmware が使う LCD / タッチ / AXP2101 / AW9523 / スピーカーは共通。
**同じバイナリがそのまま動く**前提で、以下を先に入れてある:

- 起動時に内部 I2C を probe (RTC 0x51 / IMU 0x69) して板種別を判定し、`STATUS BOARD=`
  と Log 画面に出す (`crates/hub-board/src/board.rs` → `hub-core/src/board.rs`)
- バッテリーが無い板では Log 画面の残量/充電表示を「電池なし」に切り替える
  (AXP2101 の battery-present bit でゲート。`EVT BATT` に `bat=0/1` を追加)
- 次期スタック (SE + Base LAN PoE v1.2) のピンマップは `cores3-se` feature:
  RS232M **TX=G10 / RX=G6** (ジャンパ移動が必要)、Unit NFC **Port C (G17/G18)**。
  実機未検証。CI では `cargo check` のみ通す。SE 本体に現行配線のまま載せる場合は不要

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
| [crates/hub-drivers](crates/hub-drivers) | ホストリンク / コンソール共通部 / RS232 / NTP / recorder / NFC / LAN スタブ | 低 |
| [crates/hub-ui](crates/hub-ui) | 画面処理 (状態機械 + 描画) | **高 (画面遷移の変更はここだけ)** |

`hub-*` を共有する**別バイナリ**が 4 本ある (それぞれ独立した sdkconfig /
partitions.csv を持ち、`ESP_IDF_SYS_ROOT_CRATE=<crate 名>` を付けてビルドする):

| クレート | 機 | 内容 |
|---|---|---|
| [crates/atoms3-print](crates/atoms3-print) | AtomS3 + Atomic PoE Base | 印刷ブリッジ (PDF → プリンター 9100)、Refs #38 |
| [crates/atoms3-timecard](crates/atoms3-timecard) | Atom VoiceS3R + Atomic PoE Base + Unit NFC | NFC タイムカード端末 (`kind=timecard` を WS uplink へ)、Refs #134 / #151 |
| [crates/atoms3-nfc](crates/atoms3-nfc) | AtomS3 Lite + Unit NFC | NFC ベンチ検証機。読み取りループは持たず `hub-drivers/src/nfc.rs` を呼ぶ (#146) |
| [crates/atoms3-alarm](crates/atoms3-alarm) | Atom VoiceS3R (USB のみ) | 点呼端末の警告デバイス。**運行管理者 PC のブラウザ**の heartbeat が途切れたら鳴る (下記)、Refs #135 |

**NFC の読み取りループ・ホストコンソールを新しい機へ写さないこと。**
前者は `hub-drivers/src/nfc.rs` (I2C ポートとピンを引数で受け、検知は
`NfcEvent` のコールバックで返す)、後者は `hub-drivers/src/console.rs`
(`spawn_reader` + `handle_common`) が共有実装。とくに `AUTH SET`
(device credential を NVS へ書く口) を機種ごとに増やすと provisioning が割れる。

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

## 点呼端末の警告デバイス (crates/atoms3-alarm)

**運行管理者の PC に置く据置ブザー。**運行管理者のブラウザ (alc-app トップ画面の
「運行管理者」タブ) が開いているかを見張り、閉じられたときと**着信**のときに鳴る。
機は **Atom VoiceS3R** (`crates/atoms3-timecard` と同じ本番機) で、
**USB-C で運行管理者 PC に繋ぐだけ**
— ネットワークもデバイス登録も無い (設計は
[plan/standing-devices.md](plan/standing-devices.md) §4、Refs #135)。

鳴る条件は 3 つ:

| 鳴る場面 | 端末から見えるもの | 止まるとき |
|---|---|---|
| **管理者の画面が開いていない** — タブを閉じた / 別タブへ移った / ブラウザや PC が落ちた / USB が抜けた | heartbeat が途切れる (10 秒で沈黙) | 画面を開き直す |
| **着信** — 乗務員がキオスクで点呼を始めた (signaling に room が立った) | `HB OK call=1` | 管理者が遠隔点呼に入る (`call=0`) |
| **着信を受けられない** — 管理者のブラウザが signaling の `/watch-rooms` に繋がっていない | `HB NG signaling` | 繋がり直す |

音は場面で変える: **沈黙 (繋がっていない) は 5 秒に 1 回の短い 2 連**、着信と
「受けられない」は 1.8 秒ごとの 3 連 (人を呼ぶ音は強いまま)。沈黙中に着信が来たら
5 秒待たずに 3 連へ切り替わる (2026-09-09 の実機確認でのユーザー要望)。

**ブラウザが「鳴れ」と命令する形にしていない。**運行管理者のブラウザが 3 秒ごとに
「正常」を送り続け、**途切れたら端末が自分の判断で鳴る**。命令駆動だとブラウザ / PC が
落ちたときに命令が来ず沈黙する = **一番危ないケースで鳴らない**。
沈黙を異常とみなせば、クラッシュ・タブを閉じた・フリーズ・USB 抜けを同じ形で拾える。

| 方向 | 行 | 説明 |
|---|---|---|
| 管理者のブラウザ → 端末 | `HB OK` / `HB NG <reason>` | heartbeat。3 秒ごと。末尾に任意で `call=0` / `call=1` (**着信** = 点呼の呼び出し)、意図した reload の直前は `grace=<秒>` (1〜120。**その 1 回だけ**沈黙の猶予を広げる、#192)。reason は `[a-z0-9_]+` (例 `signaling`)。**返信しない** |
| 管理者のブラウザ → 端末 | `STATUS` | `STATUS alarm state=<idle\|alarming\|muted> cause=<none\|silence\|ng:<reason>\|call> hb_age_ms=<n\|-> [grace_left_ms=<n>] VER=…` (`grace_left_ms` は `grace=` の猶予中のみ。CoreS3 は `ALARM=<state>/<cause>/<hb_age_ms>/<grace_left_ms>`、猶予外は `0`) |
| 端末 → 管理者のブラウザ | `EVT ALARM state=… cause=…` | 状態が変わるたび + 5 秒ごと (ブラウザのバナー用) |

`PING` / `HEAP` / `LOG DUMP` は共通実装 (`hub-drivers/src/console.rs`)。
**`STATUS` 応答の先頭 2 トークン `STATUS alarm` は管理者のブラウザ側の機種識別に使う**ので
変えないこと — CoreS3 と VoiceS3R は USB の VID/PID が同一で記述子では見分けられない。

| 値 | 既定 | 意味 |
|---|---|---|
| `SILENCE_MS` | 10 秒 | 最後の heartbeat からこれだけ空いたら沈黙 = 異常 (管理者の画面が閉じている。3 秒間隔の 3 回ぶん) |
| `BOOT_GRACE_MS` | 30 秒 | 起動後まだ一度も受けていないあいだの猶予 |
| `ALERT_PERIOD_MS` | 1.8 秒 | 鳴動中に警告音を出し直す周期 |
| `BANNER_MS` | 5 秒 | 状態が変わらなくても `EVT ALARM` を出し直す周期 |
| `MUTED_TICK_MS` | 5 秒 | ボタンで黙らせているあいだ、短い合図を出し直す周期 |
| `SILENCE_TICK_MS` | 5 秒 | 沈黙 (繋がっていない) で鳴動中に短い 2 連を出し直す周期 |

判定と閾値は `crates/hub-core/src/alarm.rs` (ホストでテスト済みの純粋ロジック) に
集めてあり、**`crates/atoms3-alarm` 側に数値を複製しない**。音は着信 / NG の警告が
3000Hz 200ms ×3 (止まるまで 1.8 秒ごと)、沈黙の警告が 3000Hz 60ms ×2 (止まるまで
5 秒ごと)、解消の合図が 1200Hz 150ms ×1、黙らせているあいだの合図が
3000Hz 60ms ×1 (5 秒ごと)。

**本体ボタン (G41) はトグル。** 鳴動中に押すと黙り、黙っているあいだにもう一度
押すと鳴動へ戻る (押した手応えとして即 1 回鳴る)。**黙らせても無音にはしない** —
`MUTED_TICK_MS` ごとに短い合図を出し、異常が続いていることを思い出させる
(完全な無音だと忘れられる。2026-09-09 の実機確認でのユーザー要望)。
**ボタンで黙らせたときに「直った」合図 (1200Hz) は鳴らさない** — ボタンなら異常は
続いていて人が見に行く必要があり、解消なら放置でよい。この区別を現場で音だけで
つけるため。状態遷移は `Idle ──異常──▶ Alarming ◀──ボタン──▶ Muted`、
どちらの状態からも異常が解消すれば `Idle` に戻って「直った」合図が鳴る。

書き込みは Pages の[インストーラ](docs/alarm.html) (`manifest-alarm.json`)。
焼いたあとは**運行管理者タブ → デバイス管理 → 警告デバイスを接続**でブラウザと繋ぐ。
**USB 給電なので運行管理者 PC の電源が落ちるとブザーも止まる** — 承知の上の割り切り
(plan §4.1 の「許容する穴」)。

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
