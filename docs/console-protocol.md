# ホストコンソール・プロトコル (正本)

USB Serial/JTAG 上の行指向テキストプロトコルの**仕様の正本**。
`ippoan/alc-app` (ブラウザ側) と、この repo の 5 機種すべてが従う。

**この repo の各所 (README.md / `docs/*.html` / 各 `console.rs`) にあった重複した
説明は、この 1 本を指す形に整理してある** (Refs ippoan/alc-app#353)。
機種識別の約束が `crates/atoms3-alarm/src/console.rs` の doc コメントの奥に
埋もれていたせいで、新しい機種 (測定台) を作った人に伝わらず、ブラウザ側が
「答えないこと」で機種を判定する応急処置に頼る羽目になった — 同じ穴を防ぐため、
**契約はここに 1 か所だけ書く**。

`.md` ファイルなので Pages (`assemble-pages` の `cp` 対象) には publish されない —
開発者向けの内部ドキュメント。利用者向けのインストーラ説明は `docs/*.html` 側
(機種固有の 2〜3 行 + このファイルへのリンク) に残す。

## 1. 行指向の約束

1 行 1 メッセージ (CR または LF で区切る)。ESP-IDF のログが同じコンソールに
混在するため、ホスト側は**既知の接頭辞の行だけ**を解釈すること:

```
OK  ERR  PONG  DEVICE  STATUS  EVT
```

CoreS3 (`cores3`) はこれに `FC1200` (RS232 パススルー) / `CFG` (設定
エクスポート) / `{` (BLE 測定データの JSON 行、
[ble-medical-gateway](https://github.com/ippoan/ble-medical-gateway) の
シリアル JSON 互換) が加わる。

タイムカード端末 (`timecard`) は `OTA` (シリアル OTA の応答、§6) が加わる。

Vein Station の `vein` build (`timecard`) は `VEIN` (指静脈の特徴量、§4 の
`VEIN CAPTURE`) が加わる。**この行は 2 千文字を超える** (特徴量 0x448 バイトなら
2192 文字の 16 進) — ホストは行の長さで切り捨てないこと。

★ **ファームは応答行・`EVT ` 行を必ず改行から出す** (Refs ippoan/alc-app-s3#268)。
起動直後は ESP-IDF のログ行が途中でバイトを落とすことがあり、そこへ応答が
**連結**して既知の接頭辞から外れていた (実測: `…nfc: 待受開始 port=0` +
`EVT NFC_READY port=0`)。出口 3 か所 (`console::spawn_reader` /
`host_link::drain_buffer` / `evtlog::emit`) が
[`alc_hub_common::hostout`](../crates/hub-common/src/hostout.rs) を通すので、
ホストの行分割がそこで切ってくれる。**その結果、空行が 1 つ増えることがある** —
ホスト側は空行を読み飛ばすこと (3 本とも既にそうなっている)。

行の解析そのもの (純粋・ホストでテスト済み) は
[`crates/hub-core/src/protocol.rs`](../crates/hub-core/src/protocol.rs) の
`parse_line` が持つ。副作用 (NVS 保存・応答出力) は firmware 側が担う。

## 2. `DEVICE` — 機種の名乗り

```
DEVICE <kind> VER=<version> [BOARD=<board>] FLAVOR=<flavor>
```

★ **ブラウザ側 (`ippoan/alc-app`) はこれで機種を識別する。先頭 2 トークン
(`DEVICE <kind>`) を変えないこと。** CoreS3 と VoiceS3R (警告デバイス・測定台)
は USB の VID/PID が同一 (0x303A:0x1001) で、USB 記述子では見分けられない。

`<kind>` の語彙 (auth-worker の `DEVICE_KINDS` の key と同じ):

| `<kind>` | 機種 |
|---|---|
| `cores3` | M5Stack CoreS3 / CoreS3 SE (運行者タブ) |
| `atoms3-print` | AtomS3 印刷ブリッジ |
| `timecard` | NFC タイムカード端末 |
| `alarm` | 警告デバイス (Atom VoiceS3R) |
| `bp-station` | 血圧計用 PC の測定台 (`atoms3-nfc` の VoiceS3R build) |

`BOARD=` は板種 (`cores3` / `cores3se`) が複数ある機種だけが足す。今のところ
CoreS3 だけ (`crates/hub-core/src/board.rs`)。板種は起動後に変わらないので、
状態ではなく名乗りの側に載せる。

`FLAVOR=` はビルド種別 (Refs #279)。**全機種が常に末尾に付ける** — ホストは
2 語目 (`<kind>`) を読み、それ以降は `KEY=` で拾うこと (語の位置や行末で
固定しない)。同じ `<kind>` でもビルドによって流し込むイメージが違うので、
シリアル OTA (§6) のホストはこれで選ぶ。語は各機種の crate が決める:

| `<flavor>` | ビルド |
|---|---|
| `cores3` | CoreS3 の LAN 版 (既定 feature。dev の `mem-hud` も同じ) |
| `cores3-wifi` | CoreS3 の Wi-Fi 版 (`lan` feature 無し) |
| `atoms3-print` | 印刷ブリッジ |
| `timecard` | タイムカード端末 (LAN 版) |
| `timecard-station` | 同 `station` feature (Vein Station、LAN 無し) |
| `timecard-vein` | 同 `vein` feature (`station` + 指静脈) |
| `alarm` | 警告デバイス |
| `bp-station` | 測定台 |

全機種が [`handle_common`](#5-どこに実装が在るか) の共通実装 1 本で答えるので、
新しい機種を足すときもここに書き足す必要はない。

## 3. `STATUS` — いまの状態

`DEVICE` とは別物 — **名乗りではない**。機種ごとに中身が完全に違い、
共通実装 (`handle_common`) には**含まれない** (機種ごとの `console.rs` /
`host_link.rs` がそれぞれ答える)。応答例:

| 機種 | 応答例 |
|---|---|
| `cores3` | `STATUS LAN=0 RS232=1 BLE=0 WIFI=0 ROT=0 ALARM=idle/none/-/0` |
| `atoms3-print` | `STATUS LAN=1 IP=192.168.x.x PRINTER=host:9100 VER=…` |
| `timecard` | `STATUS LAN=1 IP=192.168.x.x VER=… CLOCK=1 EPOCH=…` |
| `alarm` | `STATUS alarm state=idle cause=none hb_age_ms=1200 VER=…` |
| `bp-station` | 持たない — `ERR UNSUPPORTED (bp-station)` |

★ 警告デバイスの `STATUS alarm …` は、**かつて先頭 2 トークンで機種識別に
使われていた名残り**。互換のため文字列は変えていないが、**識別は上の
`DEVICE` を見ること** — 新しい判定コードを `STATUS` の中身に依存させないこと。
CoreS3 の `STATUS LAN=…` も同様 (行頭は互換のため変えない)。

## 4. 共通コマンド (どの機種が何に答えるか)

行の連結順は `spawn_reader` → `handle_common` → (`timecard` だけ `handle_ota_serial`)
→ `handle_omron` → `handle_pair` → 機種固有分岐 → 最後まで捌かれなければ
`ERR UNSUPPORTED (<kind>)`。

| コマンド | `handle_common` (全機種共通) |
|---|---|
| `PING` | `PONG` |
| `DEVICE` | 上記 §2 |
| `HEAP` / `HEAP DUMP` | ヒープ概況 / 詳細 |
| `LOG DUMP` | `.noinit` (または CoreS3 は PSRAM) リングの直近ログ |
| `AUTH SET/UNPAIR/STATUS/URL/TOKEN` | device credential 管理 (auth-worker `/device/pair` 系) |
| `AUTH TICKET` | 端末登録の一回券。**`cores3` のみ** (運行者 PWA が USB 越しに繋がるのはここだけ)、他は `ERR AUTH TICKET: unsupported` |
| `AUTH KEYGEN` / `AUTH PUBKEY` / `AUTH SIGN` / `AUTH SIGNBP` | 警告デバイス管理者認証用の ed25519 鍵 (Refs #205, #249) |
| `WS URL` / `WS STATUS` | cf-alc-recorder 常時接続の URL 上書き / 状態 |
| `VEIN CAPTURE` / `VEIN SAY <x>` | `ERR VEIN: unsupported` — 指静脈を積む `vein` build だけがここを素通しして自前で答える (下記) |

`AUTH SIGN` / `AUTH SIGNBP` の応答 (Refs #205, #249, #269):

| 要求 | 応答 |
|---|---|
| `AUTH SIGN <nonce>` | `AUTH SIG <pubkey> <sig>` — 署名対象は `<nonce>` のみ (管理者ログイン)。**血圧計の状態に依らず常に答える** |
| `AUTH SIGNBP <nonce>` | `AUTH SIGBP <pubkey> <sig> BP=<1\|0>` — 署名対象は `<nonce>\|bp=<1\|0>` (キオスク端末認証) |
| `AUTH SIGN` / `AUTH SIGNBP` の nonce 不正 | `ERR AUTH: bad nonce` (小文字 hex 32 文字でない) |
| 鍵が無い | `ERR AUTH: no key` |
| `AUTH SIGNBP` で**ボンド状態をまだ確認できていない** | `ERR AUTH: bp not ready` (Refs #269) |

★ `ERR AUTH: bp not ready` は「血圧計なし」ではない。BLE スキャンが一度も
回っていない窓 (ホストが `port.open()` でチップをリセットした直後がこれに当たる)
では、既定値の `false` を `bp=0` として**署名しない** — 署名すると血圧計が
繋がっている端末が「無い」と鍵付きで申告し、法定の血圧記録を省く側へ倒れる。
ファームは上限付き (6 秒) で初回スキャンを待ってから、読めなければこれを返す。
**BLE を起こさない機 (警告デバイス / 印刷ブリッジ / AtomS3 Lite build /
`OMRON BP OFF`) は待たずに `BP=0` を返す** — そこでの `false` は未確認ではなく
確定した「血圧計なし」なので、ゲートに掛けない。
ホストは `ERR AUTH` を受けたら `AUTH SIGN` へフォールバックし、`bp_bonded` を
**付けずに**上流へ渡すこと (= 未確認を `bp=0` に潰さない)。

| コマンド | `handle_omron` / `handle_pair` (BLE 血圧計を積む機だけ) |
|---|---|
| `OMRON BP ON\|OFF` / `OMRON STATUS` | Omron 血圧計 (HEM-6231T) を拾うか。呼ぶのは `cores3` / `timecard` / `bp-station` |
| `PAIR` | BLE 全ボンド消去 → 再ペアリング受付。同上 3 機種 |

指静脈 (Vein Station の `vein` build = `timecard` + `--features vein`、
ippoan/vein-match#20)。**モジュールは実機で未確認** — 手順は
[`crates/hub-core/src/vein.rs`](../crates/hub-core/src/vein.rs) の doc:

| 要求 | 応答 |
|---|---|
| `VEIN CAPTURE` | 成功: **1 行で** `VEIN CHARA <hex>` (モジュールが返した特徴量の大文字 16 進。0x448 バイトなら 2192 文字)。失敗: `ERR VEIN <reason>` |
| `VEIN SAY PLACE\|AGAIN\|ENROLLED\|FAILED` | 案内音声 (「指を置いてください」/「もう一度置いてください」/「登録完了しました」/「読み取れませんでした」) を鳴らし `OK VEIN SAY <x>`。スピーカーが起きていなければ `ERR VEIN NO_SPEAKER` |

`<reason>`: `NO_MODULE` (接続に応答なし) / `TIMEOUT` (途中で応答が途絶えた) /
`NO_FINGER` (指が置かれずモジュールが待ちを打ち切った) / `READ_FAIL` (応答の破損・
読み直しの上限) / `RC=<hex 2 桁>` (モジュールのエラーコード)。`VEIN CAPTURE` は
指を待つので**応答まで最大 15 秒強かかる** (途中経過 1 つにつき 15 秒)。続けて
送った分は順に処理する。`VEIN SAY` は読み取り中でもすぐ鳴る。

上記に無いコマンド、または対応しない機種向けのコマンドは
`ERR UNSUPPORTED (<kind>)` を返す (`<kind>` は §2 の語彙。現場の切り分け用)。
機種固有コマンド (`QR` / `MEASURE` / `PRINT` / `OTA` 等) は各機種のドキュメントを参照:

- CoreS3: [`crates/hub-drivers/src/host_link.rs`](../crates/hub-drivers/src/host_link.rs) の doc
- 印刷ブリッジ: [`crates/atoms3-print/src/console.rs`](../crates/atoms3-print/src/console.rs) の doc
- タイムカード端末: [`crates/atoms3-timecard/src/console.rs`](../crates/atoms3-timecard/src/console.rs) の doc
- 警告デバイス: [`crates/atoms3-alarm/src/console.rs`](../crates/atoms3-alarm/src/console.rs) の doc
- 測定台 (`bp-station`): `OTA` / `OTA SERIAL` を含め機種固有コマンドを持たない (共通実装のみ)

`OTA SERIAL` / `OTA CONFIRM` (§6) に答えるのは `timecard` だけ。他の機種は
`ERR UNSUPPORTED (<kind>)` を返す (`OTA ` で始まらないので、§6 のホストは
応答が無いまま時間切れで諦める)。

## 5. どこに実装が在るか

| | ファイル |
|---|---|
| 行解析 (純粋・テスト済み) | [`crates/hub-core/src/protocol.rs`](../crates/hub-core/src/protocol.rs) |
| 共通実装 (`handle_common` / `handle_ota_serial` / `handle_omron` / `handle_pair` / `start_common`) | [`crates/hub-drivers/src/console.rs`](../crates/hub-drivers/src/console.rs) |
| シリアル OTA の書き込み・確定・戻し (§6) | [`crates/hub-drivers/src/ota.rs`](../crates/hub-drivers/src/ota.rs) |
| 機種の語彙 (`HostKind`: label / `AUTH TICKET` 可否 / `BOARD=` 要否) | [`crates/hub-core/src/protocol.rs`](../crates/hub-core/src/protocol.rs) の `HostKind` |
| CoreS3 固有分 | [`crates/hub-drivers/src/host_link.rs`](../crates/hub-drivers/src/host_link.rs) |
| 印刷ブリッジ固有分 | [`crates/atoms3-print/src/console.rs`](../crates/atoms3-print/src/console.rs) |
| タイムカード端末固有分 | [`crates/atoms3-timecard/src/console.rs`](../crates/atoms3-timecard/src/console.rs) |
| 警告デバイス固有分 | [`crates/atoms3-alarm/src/console.rs`](../crates/atoms3-alarm/src/console.rs) |
| 測定台 (`bp-station`) | 固有分は無し — `start_common` をそのまま呼ぶ (`crates/atoms3-nfc/src/main.rs`) |

**新しい機種のコンソールを丸写しで作らないこと** — とくに `AUTH SET`
(device credential を NVS へ書く口) や `OMRON BP` の応答文言を機種ごとに
増やすと、provisioning や `/device/setup` の挙動が機種ごとに割れる。

## 6. シリアル OTA (`OTA SERIAL` / `OTA CONFIRM`)

LAN も Wi-Fi も無い `timecard-station` を、USB でつながった運行者 PC の
ブラウザ (キオスク PWA、`ippoan/alc-app` の `web/`) から更新する口 (Refs #279)。
ブラウザが Pages (`https://ippoan.github.io/alc-app-s3/…`) から取った app 単体
イメージを Web Serial で**動作中の app** に流し込み、app が裏スロットへ書いて
再起動する。bootloader には入れないので NVS (登録・設定) は残る。

ホスト → 端末の行は `\n` で終わり、端末 → ホストの行は CRLF で終わる。
`EVT …` やログの行が間に混ざるので、**ホストは `OTA ` で始まる行だけを見る**。

1. ホスト: `OTA SERIAL <size> <flavor>` — `<size>` はイメージのバイト数 (10 進)、
   `<flavor>` は §2 の語
2. 端末: 受け入れたら `OTA READY 4096` (数字はチャンク長)。断るときは `OTA ERR <reason>`:

   | `<reason>` | 意味 |
   |---|---|
   | `flavor` | 自分のビルドと flavor が違う |
   | `size` | 256 KiB (`MIN_IMAGE_BYTES`) 未満か、次のスロット長を超える |
   | `busy` | 別の OTA (HTTP 版の `OTA <url>` / WS の `ota` を含む) が走っている |
   | `begin` | `esp_ota_begin` に失敗した |

3. ホスト: 生のバイト列を 4096 B ずつ送る (最後は端数)。**`OTA ACK` を受けてから
   次を送る** (stop-and-wait)
4. 端末: 1 チャンクを flash に書くたびに `OTA ACK <累計バイト数>`
   - 書き込みに失敗したら `OTA ERR write`
   - 10 秒バイトが来なければ `OTA ERR timeout`
   - どちらもスロットは切り替えず、行モードへ戻る (続けて次の `OTA SERIAL` を送ってよい)
5. 端末: `size` バイトを受け切ったら esp_image を検証する
   - 成功: NVS に「確定待ち」の印を立て、`OTA OK` を返し、約 500 ms 後に再起動する
   - 失敗: `OTA ERR verify` (スロットは切り替えない)
6. 再起動後: ホストは再接続して `DEVICE` を送る → `DEVICE timecard VER=<ver> FLAVOR=timecard-station`
7. ホスト: FLAVOR が期待どおりなら `OTA CONFIRM` を送る
8. 端末: 確定待ちなら確定して印を消し、`OTA CONFIRMED` を返す
   - 確定待ちでなくても `OTA CONFIRMED` を返す (冪等)
   - 印があるのに起動から 10 分 (`OTA_VERIFY_TIMEOUT_MS`) 以内に `OTA CONFIRM` が
     来なければ、前の image へ戻す。戻った先の起動で
     `EVT OTA_ROLLED_BACK … reason=serial_unconfirmed` が出る
   - 起動時に確定待ちなら `EVT OTA_SERIAL_PENDING slot=<label> timeout_ms=600000` が出る

- 版 (`VER`) の一致は「更新するか」の判定にだけ使う。確定の条件にはしない
- 印の無い未確定 (web インストーラ / `espflash` で入れた機) には何もしない
- `READY` の後のバイトは行に分けずに受けるので、途中に `\r` / `\n` があってもよい
  (受信は #277 で無変換)

**既知の限界**: 新しい app が起動直後に落ちる (確定の口までたどり着かない) と戻らない —
web インストーラの bootloader (espflash 同梱の `boot.bin`) は rollback を持たず、
確定も戻しも app が行うため。USB はつながっているので、web インストーラで焼き直して
復旧する。
