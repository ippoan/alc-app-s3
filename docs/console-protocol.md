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

タイムカード端末 (`timecard`) と CoreS3 (`cores3`) は `OTA` (シリアル OTA の応答、
§6) が加わる。

Vein Station の `vein` build (`timecard`) は `VEIN` (指静脈の特徴量、§4 の
`VEIN CAPTURE`) が加わる。**この行は 2 千文字を超える** (特徴量 0x448 バイトなら
2192 文字の 16 進。`VEIN ENROLL` の `VEIN RECORD` 行は最長 2069 文字) — ホストは行の長さで切り捨てないこと。

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
| `cores3` | CoreS3 の LAN 版 (既定 feature) |
| `cores3-wifi` | CoreS3 の Wi-Fi 版 (`lan` feature 無し) |
| `cores3-dev` | CoreS3 の開発用 (LAN 版 + `mem-hud`。メモリ表示つき) |
| `atoms3-print` | 印刷ブリッジ |
| `timecard` | タイムカード端末 (LAN 版) |
| `timecard-station` | 同 `station` feature (Vein Station、LAN 無し) |
| `timecard-vein` | 同 `vein` feature (`station` + 指静脈) |
| `timecard-vein-gps` | 同 `vein-gps` feature (`vein` の PORT.A を Unit GPS にし、Unit NFC を中継基板の J6 (SDA=G38 / SCL=G39) へ移した版。NMEA を受けると `EVT GPS FIX …` / `EVT GPS NOFIX …` を出す) |
| `alarm` | 警告デバイス |
| `bp-station` | 測定台 |

`cores3-dev` は `lan` と `mem-hud` の両方を持つビルドだけが名乗る (`mem-hud` で `lan` 無しは
`cores3-wifi`)。**この語が入る前の開発用ビルドが入っている機体は、新しい版を入れるまで
`cores3` を名乗り続ける** (機体側では直せない)。

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
| `timecard` | `STATUS LAN=1 IP=192.168.x.x VER=… CLOCK=1 EPOCH=…` (`vein` build は行末に ` ALARM=idle/none/-/0` を足す。下の ★) |
| `alarm` | `STATUS alarm state=idle cause=none hb_age_ms=1200 VER=…` |
| `bp-station` | 持たない — `ERR UNSUPPORTED (bp-station)` |

★ 警告デバイスの `STATUS alarm …` は、**かつて先頭 2 トークンで機種識別に
使われていた名残り**。互換のため文字列は変えていないが、**識別は上の
`DEVICE` を見ること** — 新しい判定コードを `STATUS` の中身に依存させないこと。
CoreS3 の `STATUS LAN=…` も同様 (行頭は互換のため変えない)。

★ Vein Base (`timecard` の `vein` build) は、運行管理者席で Atom VoiceS3R の代わりに
**警告デバイスを兼ねる**。`HB OK` / `HB NG <reason>` / `HB OFF` に CoreS3 と同じく答え
(初回の `HB OK` で武装し、途切れたら鳴る。`HB OFF` は `OK HB OFF`、他は返信しない)、
鳴動の状態は `STATUS` 行末の `ALARM=…` (CoreS3 と同じ欄) で渡す。`EVT ALARM` の行は出さない。
heartbeat を受けない機体 (キオスクの Vein Station) は武装しないので鳴らない

## 4. 共通コマンド (どの機種が何に答えるか)

行の連結順は `spawn_reader` → `handle_common` → (`timecard` と `cores3` だけ
`handle_ota_serial`) → `handle_omron` → `handle_pair` → 機種固有分岐 → 最後まで
捌かれなければ `ERR UNSUPPORTED (<kind>)`。CoreS3 は Improv のバイナリフレームを
混ぜるので `spawn_reader` の代わりに自前の reader (`host_link.rs`) を持つが、
以降の連結は同じ。

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
| `VEIN CAPTURE` / `VEIN SAY <x>` / `VEIN FINGER?` / `VEIN ENROLL` / `VEIN IDENTIFY` / `VEIN SYNC …` / `VEIN TEST …` | `ERR VEIN: unsupported` — 指静脈を積む `vein` build だけがここを素通しして自前で答える (下記) |

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
BLE ループが一定時間 (約 5 分) 先頭に戻っていないときも、確認できていないと答える
(`ERR AUTH: bp not ready`。WS 下り command `bp_status` は `{"bp_read":false}`)。
このときは待たずに返す (Refs ippoan/alc-app#401)。
scan や GATT の待ちが戻らなくなる止まり方そのものは直していない — 止まったことがこの応答で分かるだけ。
**BLE を起こさない機 (警告デバイス / 印刷ブリッジ / AtomS3 Lite build /
`OMRON BP OFF`) は待たずに `BP=0` を返す** — そこでの `false` は未確認ではなく
確定した「血圧計なし」なので、ゲートに掛けない。
ホストは `ERR AUTH` を受けたら `AUTH SIGN` へフォールバックし、`bp_bonded` を
**付けずに**上流へ渡すこと (= 未確認を `bp=0` に潰さない)。

| コマンド | `handle_omron` / `handle_pair` (BLE 血圧計を積む機だけ) |
|---|---|
| `OMRON BP ON\|OFF` / `OMRON STATUS` | Omron 血圧計 (HEM-6231T) を拾うか。呼ぶのは `cores3` / `timecard` / `bp-station` |
| `PAIR` | BLE 全ボンド消去 → 再ペアリング受付。同上 3 機種 |
| `BP UNBOND` | 血圧計として記録した 1 台分のボンドだけを外す依頼 (Refs ippoan/alc-app#401)。応答は `OK BP UNBOND` (受理 = BLE ループへ依頼を出した) / `ERR BP UNBOND: busy` (OTA 中・点呼中で断った)。結果は `EVT BP_UNBOND ok\|none\|err` で**非同期に**出る (BLE ループの次の周で処理するので `OK` の直後とは限らない)。`PAIR` との違い = 他の機器のボンドは残し、ペアリング受付も開かない。**`cores3` だけ** (CoreS3 固有分 `host_link.rs` が捌く。他機は `ERR UNSUPPORTED (<kind>)`) |
| `CRASH TEST abort\|fault` | **開発用ビルド (`FLAVOR=cores3-dev`) の `cores3` だけ**が受ける試験の口 (Refs ippoan/alc-app#403)。`OK CRASH TEST <種類>` を返してから、C の `abort()` を呼ぶ (`abort`) / 不正な番地へ書き込んで CPU 例外を起こし (`fault`)、**わざと落ちて再起動する**。次の起動で `EVT BOOT reset=panic` の前後に `EVT CRASH_INFO …` (README の送信イベントの表) が出ることを確かめるためのもの。ほかの引数は `ERR CRASH TEST`。ほかのビルド・機種はこの口を持たず、今までどおり `ERR 不明なコマンド: CRASH` |

指静脈 (Vein Station の `vein` build = `timecard` + `--features vein`、
ippoan/vein-match#20)。**モジュールは実機で未確認** — 手順は
[`crates/hub-core/src/vein.rs`](../crates/hub-core/src/vein.rs) の doc:

| 要求 | 応答 |
|---|---|
| `VEIN CAPTURE` | 成功: **1 行で** `VEIN CHARA <hex>` (モジュールの登録データ (1:N のテンプレート、先頭 `DEEDDEED`) から取り出した 0xBDBD の特徴量 1 件の大文字 16 進。常に 0x448 バイト = 2192 文字。取り出せなければ `READ_FAIL`)。失敗: `ERR VEIN <reason>` |
| `VEIN SAY PLACE\|AGAIN\|ENROLLED\|FAILED` | 案内音声 (「指を置いてください」/「もう一度置いてください」/「登録完了しました」/「読み取れませんでした」) を鳴らし `OK VEIN SAY <x>`。スピーカーが起きていなければ `ERR VEIN NO_SPEAKER` |
| `VEIN FINGER?` | 指の有無を 1 回だけ問い合わせる: `VEIN FINGER 1` (有る) / `VEIN FINGER 0` (無い) / `ERR VEIN UNSUPPORTED` (モジュールが `01 10` = コマンド非対応を返した) / `ERR VEIN <reason>` (下の `<reason>`)。**実機で指の状態コマンドが通るかを確かめる口** (下の「指の見張り」) |
| `VEIN ENROLL <id>` | **本番の登録** (照合をモジュールで行う M-b、ippoan/alc-app#436)。`<id>` は **10〜100** (下の「ID の分け方」。範囲外・数字でないもの・余分な引数は `ERR VEIN: ENROLL の ID は 10〜100 の数字だけです`)。手順: 接続 → `<id>` を CLEAR → ENROLL (撮り直しは **1 回置くごとに最大 5 回** (`bData[10]` = 5、1 枚撮れるたびに数え直す。2 回で 4 回置く全体を数えていたため、どこかで 3 回目の撮り直しに入ると登録ごと `RC=11` で終わっていた)。**テンプレートは 4 本** (`bData[5]` = 4。メーカーの試験ソフトは 3 本、仕様書は「最大 3 本」と書くがこの機種は 4 本を受け付ける) — 読み取りの作業用は 1 本のまま。撮るたびに「離して」→ 置き直しがあり、撮れるたびにビープが鳴る) → READ_ENROLL + READ_DATA で記録全体 (実機 8156 バイト、先頭 `DEEDDEED`) を読む。**登録の後の確認の照合はしない** (仕様は「最初の登録は 4 回置く」。同じ指かはモジュールが撮影ごとに見る、`01 09` で撮り直し)。置き直しのたびに端末が「もう一度置いてください」を鳴らし、`EVT VEIN PLACE <回目> <合計>` (回目は撮影の 2〜4 枚目 = モジュールが次の指を待ち始めたとき、合計は 4) を 1 行出す。成功: 記録を 1024 バイトずつ `VEIN RECORD <id> <offset> <大文字 16 進>` (1 行最長 2069 文字。8156 バイトなら 8 行) で出し、最後に `VEIN RECORD <id> END <総バイト数>`。**行はモジュールから全部読み終えてから続けて出す** (途中に `ERR` は挟まらない)。ホストは END を見てから offset の連続と総バイト数を確かめて使う。登録の後で記録を読めなかったら `<id>` を CLEAR してから `ERR VEIN <reason>` (渡せない登録をモジュールに残さない)。`ERR VEIN RETAKE` はもう出さない。4 回置くので応答まで最長 1 分ほど |
| `VEIN IDENTIFY` | **本番の照合**。モジュールの 1:N 照合 (VERIFY `0`、§2.2.21) を 1 回行う。**VERIFY の前に指の状態を 50ms ごとに問い合わせ、「有る」を見てから 200ms 待つ** (触れた瞬間に撮ると置ききる前になり、点数の付かない落ち方が増えた。非対応・失敗なら待たずに送る。指が来なければ指待ちの時間ほどで諦めてそのまま送る)。指を待つ途中経過 (置いて・離して) は読み取りと同じに扱う。当たれば `VEIN IDENTIFIED <id>` (最終応答の `bData[1..5]` LE)。**照合の直後 (当たっても外れても) にモジュールの動作記録 (GET_DEBUG `0x26` → READ_DATA 種別 `0x26`、4096 バイトの文字。`VEIN TEST RAW` で読めていたもの) を 1 回だけ読み**、**最後の照合の塊 (最後の `PS_GenChar OK` の行より後) だけ**から点数を読み、読めれば `VEIN IDENTIFIED <id> SCORE=<点数> TH=<s1> VIA=FV|FP THS=<s0>,<s1>,<s2>` と続ける (記録は起動からの追記。実機の塊は、静脈で一致なら `1:1->A:<id>, <n>,<s0>,<s1>,<s2>,<点数>` / `PS_Search OK:FV=<id>(<id>),FP=<id>` / `USER:<id>, th[s0, s1, s2], ...`、指紋互換だけで一致なら `->A:` の行が無く `PS_Search OK:FV=0(0),FP=<id>` / `th[0, 0, 0]`。点数は `->A:` の行の ID の後ろの 5 つ目 (1:N は `1:N->A:<id>, <n>,<s0>,<s1>,<s2>,<点数>,<1>` と点数の後ろにもう 1 つ付く) で、無ければ 0。VIA は `PS_Search` の FV が当たった ID なら FV、FP だけなら FP。合格線が 3 段のどれかは未確定 (68 点が th[57, 67, 78] で静脈で通った) なので TH は真ん中、THS で全部出す)。記録が返らない・読めない形なら付け足さず今までの行のまま (待ちは 300ms ずつに縮め、失敗したら入力を捨てる。読み出しで応答が 0.7 秒ほど遅れる)。古いアプリは先頭の `<id>` だけ読めばよい。不一致 (`01 0C`) は `VEIN NOMATCH`。記録が読めれば、最後の塊の `->A:` の行 (どの ID でも) のうち点数が最も高いものを ` BEST=<id> SCORE=<点数> THS=<s0>,<s1>,<s2>` と続け (閾値は `->A:` の行の点数の前の 3 つ)、`->A:` の行が 1 つも無ければ (静脈が点数の段階まで進まなかった) ` SCORE=0` を続ける。**不一致と、指紋互換だけで当たった回 (`VIA=FP`) は、応答の行の前に `VEIN MATCHLOG <最後の塊の行を ` \| ` でつないだもの>` を 1 行出す** (表示できる ASCII だけ、400 文字で切る。不一致の塊の形を後から読むため。照合の応答を待つ側は接頭辞で待つので読み飛ばせばよい)。`01 10` は `ERR VEIN UNSUPPORTED`、ほかは `ERR VEIN <reason>`。引数を付けると `ERR VEIN: IDENTIFY に引数は要りません`。撮り直しのたびに `EVT VEIN AGAIN <理由>` が出て指を待ち直すので、応答まで最長 50 秒弱 (指待ち 15 秒 × 3 回) |
| `VEIN VERIFY <id>` | **本番の 1:1 照合** (免許証で本人が決まった後に使う)。`<id>` は 10〜100 (範囲外・数字でないもの・余分な引数は `ERR VEIN: VERIFY の ID は 10〜100 の数字だけです`)。手順は `VEIN IDENTIFY` と同じ (指が置ききるのを待ってから VERIFY `<id>`)。当たれば `VEIN VERIFIED <id>` (`VEIN IDENTIFY` と同じく、点数が読めれば ` SCORE=<点数> TH=<s1> VIA=FV|FP THS=<s0>,<s1>,<s2>` が続く)、不一致 (`01 0C`) は `VEIN NOMATCH` (点数と `VEIN MATCHLOG` の行も `VEIN IDENTIFY` と同じ)、ほかは `VEIN IDENTIFY` と同じ |
| `VEIN SYNC WRITE <id> <offset> <hex>` | **同期の書き込み** (サーバーの記録をモジュールの `<id>` (10〜100) へ書く)。組み立て中のデータへ足す。`<hex>` は 1 行 **192 バイトまで** (384 文字)。`<offset>` は今までに受けたバイト数 (最初は 0 で、0 は組み立てを最初からやり直す)。飛び・重なり・組み立て中の ID が違うときは `ERR VEIN WRITE_SEQ`、合計が 0x2000 バイトを超えると `ERR VEIN WRITE_TOO_BIG` で、どちらも組み立て中のものを捨てる。成功: `VEIN SYNC WRITE <id> <次の offset>`。**ホストは応答を待ってから次の行を送る** (console の 1 行は 512 バイトまで、USB の受信リングは 1024 バイトのため。超えた行は黙って捨てられる)。形の誤り (ID・offset・hex・192 バイト超) は `ERR VEIN: SYNC は …` |
| `VEIN SYNC WRITE <id> END` | 組み立てたデータの先頭が `DE ED DE ED` であることを確かめ (違えば `ERR VEIN WRITE_HEADER`、書かない)、WRITE_ENROLL (`0x23`、§2.2.27) + WRITE_DATA (`0x21`、§2.2.25) で `<id>` へ書く (**フラッシュに書く**。512 バイトずつ、応答が成功でなければ 3 回まで送り直す)。成功: `VEIN SYNC WRITTEN <id> <バイト数>`。組み立て中のものが無い / ID が違えば `ERR VEIN WRITE_SEQ` |
| `VEIN SYNC CLEAR <id>` | `<id>` (10〜100) の登録を消す (CLEAR_ENROLL)。成功: `VEIN SYNC CLEARED <id>`、失敗: `ERR VEIN <reason>` (未登録の ID はモジュールの失敗コードがそのまま `RC=…`) |
| `VEIN SYNC INFO` | モジュールの登録人数と上限を読む (GET_ENROLL_INFO、§2.2.18)。成功: `VEIN SYNC INFO USERS=<登録人数> MAX=<上限>` (この機種は上限 100)。ブラウザが「書いたはずの人数」と比べ、モジュールを入れ替えた・初期化した (中身が空になった) ことに気づくのに使う。生体データは含まない |
| `VEIN SECURITY [<0-2>]` | **照合の厳しさ (安全レベル)**。引数なしは読む (GET_SYSTEM_INFO `0x03` の `bData[5]`、§2.2.3)、`0`〜`2` は書いてから読み直す (XG_CMD_SET_SECURITYLEVEL `0x07`、§2.2.7)。成功: `VEIN SECURITY <レベル>`。0 が最も緩く (本人を落とす率 0.001%・他人を通す率 0.001%)、2 が最も厳しい (0.05%・0.0001%)。動作記録の `th[s0, s1, s2]` はレベル 0〜2 の閾値と見られる (実機の合否が s1 と合う = 工場の既定は 1)。`01 10` は `ERR VEIN UNSUPPORTED`、ほかの失敗は `ERR VEIN <reason>`。3 以上・数字でないもの・余分な引数は `ERR VEIN: SECURITY は引数なし (読む) か 0〜2 です`。生体データは含まない |
| `VEIN TEST ENROLL <id>` | **試験用**。モジュール自身の登録に ID `<id>` (**2〜9 だけ**。1 は読み取りの作業用で毎回消されるので不可。範囲外・数字でないものは `ERR VEIN: TEST の ID は 2〜9 の数字だけです`) で 1 回登録する: ID を CLEAR → ENROLL (読み取りと同じ引数) で止め、READ_ENROLL も削除もしない (モジュールに残る)。成功: `VEIN TEST ENROLLED <id>`、失敗: `ERR VEIN <reason>`。指を待つので最大 15 秒強 |
| `VEIN TEST VERIFY <id>` | **試験用**。モジュール自身の照合 (`0x17` VERIFY) を ID 指定で 1 回行う。**`<id>` は 0 か 2〜9 か本番の 10〜100 (VERIFY だけ 0 と 10〜100 を許す — 照合は読むだけで登録を書き換えないので、同期で書いた記録を 1:1 で試せる。範囲外・数字でないものは `ERR VEIN: VERIFY の ID は 0 か 2〜100 の数字だけです`)。0 以外は 1:1、`0` は 1:N (登録済み全員から探す、§2.2.21)。1:N の成功時は `DATA` の先頭 4 バイト (LE) が当たった ID**。指を待つ途中経過 (置いて・離して) は読み取りと同じに扱う。最終応答: `VEIN TEST VERIFY <id> RC=<応答データの 1 バイト目、16 進 2 桁> DATA=<残りの 16 進>` (`DATA` は先頭 32 バイト = 64 文字で切る)。`01 10` (非対応) は `ERR VEIN UNSUPPORTED`、他は `ERR VEIN <reason>`。**引数の並び (`bData[0..4]` = ID LE) と応答の形は推測 — 実機で確認**。VERIFY が指を待たず即座に「指が無い」系を返す機種かどうかも**実機で確認** (そうなら FINGER_STATE で指を待ってから送る形に直す) |
| `VEIN TEST IMAGE` | **試験用**。撮影して生画像を読む: READ_IMAGE (`0x24`、送る `bData` は 6 バイトの 0) で撮影し、応答の `bData[1..3]` = 幅・`[3..5]` = 高さ (LE) を受けて、READ_DATA の種別 `0x24` で幅 × 高さバイト (8bit グレースケール、320×240 なら 76,800 バイト) を 512 バイトずつ読む。指を待つ途中経過 (置いて・離して・撮り直し) は登録と同じに扱う。成功: 画素を 1024 バイトずつ `VEIN TEST IMAGE <offset> <大文字 16 進>` で出し (320×240 なら 75 行)、最後に `VEIN TEST IMAGE END <幅> <高さ> <総バイト数>`。**行は読み終えてから続けて出す** (57600 bps で読み出しに 15 秒ほど)。`01 10` (非対応) は `ERR VEIN UNSUPPORTED`、他は `ERR VEIN <reason>`。引数を付けると `ERR VEIN: IMAGE に引数は要りません`。**生体データ (指の画像) そのもの** — 個体ごとの写り方の違いを目で比べるためだけの口で、運用の画面からは送らない。**`0x24` は仕様書の表に無く、DLL の解析からの推測。この機種の実機で通るかは未確認** (ippoan/alc-app#436) |
| `VEIN TEST DEBUG` | **試験用**。接続してモジュールの動作記録 (GET_DEBUG `0x26` → READ_DATA 種別 `0x26`、4096 バイトの文字) を読み、**そのまま行ごとに** `VEIN TEST DEBUG <行>` で返し、最後に `VEIN TEST DEBUG END <行数>`。NUL は落とし、空行は飛ばし、1 行は 200 文字で切る。照合 (`VEIN IDENTIFY` / `VEIN VERIFY`) の点数 (`SCORE=`) をどの行から取っているかを実機の行で確かめるための口。送るのは接続と GET_DEBUG/READ_DATA だけ。引数を付けると `ERR VEIN: DEBUG に引数は要りません`。失敗は `ERR VEIN <reason>` |
| `VEIN TEST READ <id>` | **試験用**。`<id>` は 2〜100 (それ以外・余分な引数は `ERR VEIN: READ の ID は 2〜100 の数字だけです`)。接続して ID の登録データ (READ_ENROLL → READ_DATA。`VEIN ENROLL` の読み出しと同じ) をそのまま読み、`VEIN ENROLL` と同じ `VEIN RECORD <id> <offset> <HEX>` 行と `VEIN RECORD <id> END <総バイト数>` で返す。照合を重ねるとモジュールが記録 (学習記録を含む) を書き換えるかを、前後で読み比べて確かめるための口。書き換えはしない。未登録・失敗は `ERR VEIN <reason>` |
| `VEIN TEST SYSINFO` | **試験用**。モジュールの設定を GET_SYSTEM_INFO (`0x03`、送る `bData` 無し) で読み、応答のデータ部をそのまま返す。成功: `VEIN TEST SYSINFO RC=<1 バイト目、16 進 2 桁> DATA=<残り 15 バイトの 16 進>` (失敗コードも解釈せずにこの形で返す)。`01 10` (非対応) は `ERR VEIN UNSUPPORTED`、他は `ERR VEIN <reason>`。引数を付けると `ERR VEIN: SYSINFO に引数は要りません`。生体データは含まない。**番号と応答の並びは推測** — 照合の厳しさ (安全レベル) が個体で違うかを比べるために、データ部を実機で読む (ippoan/alc-app#436) |
| `VEIN TEST RAW <待ち ms> <16 進>` | **試験用**。16 進のバイト列 (1〜192 バイト) を**枠も和も付けずにそのまま** UART へ送り、返ってきたバイトをそのまま返す。接続 (CONNECTION) もしないので、送る側が XG の 24 バイト枠や指紋互換の `EF 01` 枠を組み立てる。最初の 1 バイトを `<待ち ms>` (0〜30000) まで待ち、その後はバイトの間が空くか 1024 バイトたまるまで読む。成功: `VEIN TEST RAW RX=<大文字 16 進>` (何も返らなければ `RX=` のまま)。形が違えば `ERR VEIN: RAW <待ち ms (0〜30000)> <16 進 (1〜192 バイト)> です`。仕様書に節の無い命令 (`GET_DEBUG 0x26`・`WRITE_IMAGE 0x25`・指紋互換の `EF 01` 形式) が通じるかを確かめるための口で、`VEIN TEST RAWQ` でためた分があれば**その後ろにつないで 1 回で流す** (送ったら空になる)。運用の画面からは送らない。**モジュールに何でも送れる** — 登録の消去や設定の書き換えも通るので、試験台の個体にだけ使う (ippoan/alc-app#436) |
| `VEIN TEST RAWQ <16 進>` | **試験用**。16 進のバイト列 (1〜192 バイト) を**送らずに端末の中にためる**。成功: `VEIN TEST RAWQ <ためたバイト数>`。合わせて 4096 バイトを超えたら全部捨てて `ERR VEIN: RAWQ は合わせて 4096 バイトまでです (ためた分は捨てた)`。形が違えば `ERR VEIN: RAWQ <16 進 (1〜192 バイト)> です`。UART には触れない。次の `VEIN TEST RAW` が、ためた分の後ろに自分の分をつないで流す。**1 行 = 1 枠のまま持ち**、`VEIN TEST RAWGAP` で決めた間隔を枠の間に置く。指紋互換の DownChar (`EF 01` の `0x09`) はモジュールが命令の直後にデータが続くのを待ち、間が空くと受信を打ち切る (`SynoRevDataEndPkt TIMEOUT`) — コンソールの 1 行 (512 文字) には 1 人分 (700〜850 バイト) が載らないので、ためてからまとめて流す (ippoan/alc-app#436) |
| `VEIN TEST RAWGAP <ms>` | **試験用**。次からの `VEIN TEST RAW` で、ためた枠 (`VEIN TEST RAWQ` の 1 行) の間に置く間隔を 0〜1000 ms で決める (起動時は 0 = 詰めて流す)。成功: `VEIN TEST RAWGAP <ms>`。形が違えば `ERR VEIN: RAWGAP <ms (0〜1000)> です`。UART には触れない。間隔は枠を UART の送信バッファに渡した時点から数える (57600bps で 1 バイト約 0.17 ms の送出時間を含む)。指紋互換の DownChar は、枠を詰めて流すと 1 枠ずつ取りこぼし、間が空きすぎると打ち切る — 通る間隔を探すための口 (ippoan/alc-app#436) |
| `VEIN TEST CAPACITY` | **試験用**。登録ユーザー数と登録できる上限を GET_ENROLL_INFO (`0x14`、§2.2.18、送る `bData` 無し) で読む。成功: `VEIN TEST CAPACITY USERS=<bData[1..5] LE> MAX=<bData[9..13] LE>`、失敗: `ERR VEIN <reason>`。引数を付けると `ERR VEIN: CAPACITY に引数は要りません`。生体データは含まない。**応答の読み方は仕様書どおりで、WS-FVS10 の実機では未確認** |
| `VEIN TEST CLEAR <id>` | **試験用**。ID `<id>` の登録を消す (CLEAR_ENROLL)。成功: `VEIN TEST CLEARED <id>`、失敗: `ERR VEIN <reason>` (未登録の ID はモジュールの失敗コードがそのまま `RC=…` で返る) |

**ID の分け方** (モジュールの上限は 100 人): **1** = 読み取り (`VEIN CAPTURE`) の作業用 / **2〜9** = 試験 (`VEIN TEST ENROLL\|VERIFY\|CLEAR`) / **10〜100** = 本番 (`VEIN ENROLL` / `VEIN SYNC …`)。確認タブの試験が本番の登録を上書きしないよう重ねない。`VEIN IDENTIFY` (VERIFY 0) は ID を取らず、登録済み全員から探す。

**`VEIN TEST` の INFO / COPY / WRITE / REBUILD は ippoan/alc-app#436 の切り分けが済んだので外した** (0.1.6)。WRITE は本番の口 `VEIN SYNC WRITE` として残る。

- **生体データ (記録・特徴量) を出すのは `VEIN RECORD` 行 (と旧経路の `VEIN CHARA` 行) だけ**: 受け取った `SYNC WRITE` の hex・読んだ記録は `ERR` の行・crash_log・リング・EVT・PWALOG・ログのいずれにも書かない (エラー文は理由だけで受け取った語を含めない。コマンドの `Debug` は大きさだけ)
- 引数の並びは Waveshare の公開仕様 Ver 2.3 のとおり (READ_ENROLL / VERIFY / CLEAR は `bData[0..4]` = ID LE、WRITE_ENROLL は ID LE + 大きさ LE の 8 バイト、WRITE_DATA は種別 + オフセット + 大きさの 9 バイト + 本体 + 本体の和)。VERIFY (1:1・1:N) は 0.1.5 の実機で確認済み。**推測・実機で確認**: WRITE_DATA の「種別」に WRITE_ENROLL の `0x23` を入れること (仕様の表に従ったが、本文の備考は READ_ENROLL のまま)、WRITE_DATA 1 塊の応答が 24 バイトのパケットで来ること、書き込みに `data_ms` (1 秒) で足りること、書き戻した記録が VERIFY で当たること

試験用のコマンドは**モジュール自身の登録と照合で撮像を確かめる診断**で、運用の画面からは送らない。開発用の端末の画面の `window.alcVein` からだけ送る (機体の撮像・モジュールの照合の確かめは別の機体・別の指でも今後要るため、口は残す)。指の見張りと同じ UART なので、同じ読み取りスレッドの列で直列に処理する。

`<reason>`: `NO_MODULE` (接続に応答なし) / `TIMEOUT` (途中で応答が途絶えた・`VEIN ENROLL` で指が 10 秒離れない) /
`NO_FINGER` (指が置かれずモジュールが待ちを打ち切った) / `READ_FAIL` (応答の破損・
読み直しの上限・登録データから特徴量 1 件を取り出せない) / `RC=<hex 2 桁>` (モジュールのエラーコード) / `UNSUPPORTED` (VERIFY・指の状態がコマンド非対応 `01 10`) / `RETAKE` (`VEIN ENROLL` の置き直し照合が外れた。登録は消してある) / `WRITE_SEQ`・`WRITE_TOO_BIG`・`WRITE_HEADER` (`VEIN SYNC WRITE` の組み立ての誤り)。`VEIN CAPTURE` は
指を待つので**応答まで最大 15 秒強かかる** (途中経過 1 つにつき 15 秒)。続けて
送った分は順に処理する。`VEIN SAY` は読み取り中でもすぐ鳴る。

**指の見張り** (Refs ippoan/alc-app#436): `vein` build は依頼 (`VEIN CAPTURE` / `VEIN FINGER?`) の無い間、
200ms ごとにモジュールへ指の有無を問い合わせ、**「無い → 有る」に変わったときだけ**
`EVT VEIN FINGER` を 1 行出す (画面はこれを受けて照合 = `VEIN CAPTURE` を始める。ファームは撮らない)。

| 行 | 意味 |
|---|---|
| `EVT VEIN FINGER` | 指が置かれた。指が離れる (「無い」を見る) まで次は出ない。`VEIN CAPTURE` の後も、いったん「無い」を見るまで出ない。起動時から置かれている指でも出ない |
| `EVT VEIN AGAIN <理由>` | 読み取り・照合・登録の途中で、モジュールが撮り直しを求めた。`<理由>` はモジュールの失敗コードの大文字 16 進 2 桁 (`11` = 静脈が写らない・`09` = 同じ指でない)。同時に端末が「もう一度置いてください」を鳴らし、モジュールは次の指を最長 15 秒待つ。撮り直しは照合・読み取りでは 1 回の要求につき最大 2 回、本番の登録 (`VEIN ENROLL`) では 1 回置くごとに最大 5 回 (超えると `ERR VEIN <reason>`)。画面はこれを見て案内を出し、応答の待ち時間を延ばす。生体データは含まない |
| `EVT VEIN PLACE <回目> <合計>` | `VEIN ENROLL` の途中で置き直してもらうとき。`<合計>` は置く回数の合計 (= 撮影の本数 4。登録の後の確認の照合はしない)。`<回目>` は撮影の 2〜4 枚目で、モジュールが次の指を待ち始めたときに出す (撮れた直後に決め打ちで鳴らすと早すぎる)。同時に端末が「もう一度置いてください」を鳴らす。生体データは含まない |
| `EVT VEIN FINGER UNSUPPORTED` | モジュールが指の状態コマンドに `01 10` (コマンド非対応) を返した。**見張りをやめる** (再起動まで出ない)。1 回だけ出る |

- 非対応と決めるのは `01 10` だけ。時間切れ・壊れた応答・他のエラーコード・想定外のデータは一時的な失敗として
  見張りを続ける (連続 3 回失敗したら間隔を 5 秒に延ばし、成功したら 200ms に戻す。`EVT` は出さない)
- 接続 (CONNECTION) は見張りを始めるとき・失敗の後・`VEIN CAPTURE` の後だけ送り、問い合わせのたびには送らない
- 読み取り中は問い合わせない (UART に触るのは vein スレッド 1 本で、直列)
- **コマンド番号と応答の読み方は推測**: 指の状態コマンドを XG 系の `0x10` とし、成功 (`bData[0]` = 0) の
  `bData[1]` が 1 なら有る・0 なら無いと読む。仕様書 (Communication Protocol Ver 2.3) の本文が repo に無く、
  WS-FVS10 で通るかは未確認。`VEIN FINGER?` を打って確かめる (`UNSUPPORTED` ならこの機種では見張れない。
  `RC=` や `READ_FAIL` なら番号か読み方が違う)

上記に無いコマンド、または対応しない機種向けのコマンドは
`ERR UNSUPPORTED (<kind>)` を返す (`<kind>` は §2 の語彙。現場の切り分け用)。
機種固有コマンド (`QR` / `MEASURE` / `PRINT` / `OTA` 等) は各機種のドキュメントを参照:

- CoreS3: [`crates/hub-drivers/src/host_link.rs`](../crates/hub-drivers/src/host_link.rs) の doc
- 印刷ブリッジ: [`crates/atoms3-print/src/console.rs`](../crates/atoms3-print/src/console.rs) の doc
- タイムカード端末: [`crates/atoms3-timecard/src/console.rs`](../crates/atoms3-timecard/src/console.rs) の doc
- 警告デバイス: [`crates/atoms3-alarm/src/console.rs`](../crates/atoms3-alarm/src/console.rs) の doc
- 測定台 (`bp-station`): `OTA` / `OTA SERIAL` を含め機種固有コマンドを持たない (共通実装のみ)

`OTA SERIAL` / `OTA CONFIRM` (§6) に答えるのは `timecard` と `cores3`
(Refs ippoan/alc-app#403) と `alarm` (Refs ippoan/alc-app#425) だけ。他の機種は
`ERR UNSUPPORTED (<kind>)` を返す
(`OTA ` で始まらないので、§6 のホストは応答が無いまま時間切れで諦める)。
**この対応より前の版の CoreS3 も `ERR UNSUPPORTED (cores3)` を返す** — その機は
一度 web インストーラか `OTA <url>` / WS の `ota` で上げてからでないと、§6 では
更新できない。**この対応より前の版の警告デバイスも `ERR UNSUPPORTED (alarm)` を返す** —
ネットワークを持たないので、一度 web インストーラ (docs/alarm.html) で焼き直してから
でないと、§6 では更新できない。

## 5. どこに実装が在るか

| | ファイル |
|---|---|
| 行解析 (純粋・テスト済み) | [`crates/hub-core/src/protocol.rs`](../crates/hub-core/src/protocol.rs) |
| 共通実装 (`handle_common` / `handle_ota_serial` / `feed_serial_ota` / `handle_omron` / `handle_pair` / `start_common`) | [`crates/hub-drivers/src/console.rs`](../crates/hub-drivers/src/console.rs) |
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

LAN も Wi-Fi も無い `timecard-station` と、キオスクの PC に USB でつながる CoreS3
(`cores3` / `cores3-wifi` / `cores3-dev`、Refs ippoan/alc-app#403)、運行管理者の PC に
USB でつながる警告デバイス (`alarm`、Refs ippoan/alc-app#425) を、その PC の
ブラウザ (キオスク PWA、`ippoan/alc-app` の `web/`) から更新する口 (Refs #279)。
ブラウザが Pages (`https://ippoan.github.io/alc-app-s3/…`) から取った app 単体
イメージを Web Serial で**動作中の app** に流し込み、app が裏スロットへ書いて
再起動する。bootloader には入れないので NVS (登録・設定) は残る。

ホスト → 端末の行は `\n` で終わり、端末 → ホストの行は CRLF で終わる。
`EVT …` やログの行が間に混ざるので、**ホストは `OTA ` で始まる行だけを見る**。

0. ホスト: **書き込みを始める前に** `OTA CONFIRM` を送り、応答
   `OTA CONFIRMED RX=<受信リングのバイト数>` (例 `OTA CONFIRMED RX=8192`) の `RX=` を読む。
   `RX=` は機種ごとで、端末が実際に取れた大きさを名乗る。シリアル OTA を受ける機種
   (CoreS3 / `timecard` / `alarm`) は 8192 (確保に失敗した機は 1024 に落ちて、そう名乗る)
   - **`RX=` がチャンク長 (4096) 以上のときだけ `OTA SERIAL` を送る**
   - `RX=` が無い (`OTA CONFIRMED` だけを返す古い版) か、チャンク長より小さい機には
     `OTA SERIAL` を送らない。配布ページ (web インストーラ) から書き直す。
     受信リングが満杯のとき、端末は受けたバイトを捨てる (ホストへ待てとは伝わらない)
     ので、チャンクが揃わず `OTA ERR timeout` になる。小分けにして間を置いても
     落ちることがあり、時間も掛かりすぎる
   - `OTA CONFIRM` は確定待ちの機では確定もする (8.)。確定待ちでなければ何も変えない。
     **ホストは `DEVICE` で FLAVOR を確かめた後に探りを送る** (確定の条件 7. と同じ)
1. ホスト: `OTA SERIAL <size> <flavor>` — `<size>` はイメージのバイト数 (10 進)、
   `<flavor>` は §2 の語
2. 端末: 受け入れたら `OTA READY 4096 RX=<受信リングのバイト数>` (例 `OTA READY 4096 RX=8192`。
   最初の数字はチャンク長、`RX=` は 0. と同じ値)。`RX=` の無い `OTA READY 4096` を返す
   古い版も在る。断るときは `OTA ERR <reason>`:

   | `<reason>` | 意味 |
   |---|---|
   | `flavor` | 自分のビルドと flavor が違う |
   | `size` | 256 KiB (`MIN_IMAGE_BYTES`) 未満か、次のスロット長を超える |
   | `busy` | 別の OTA (HTTP 版の `OTA <url>` / WS の `ota` を含む) が走っている。**CoreS3 は点呼中もこれで断る** (書き込みは始めない。点呼が終わってから送り直す) |
   | `begin` | `esp_ota_begin` に失敗した |

3. ホスト: 生のバイト列を 4096 B ずつ送る (最後は端数)。**`OTA ACK` を受けてから
   次を送る** (stop-and-wait)
   - チャンクは 1 回の書き込みで送ってよい (0. で `RX=` がチャンク長以上と確かめてある)
4. 端末: 1 チャンクを flash に書くたびに `OTA ACK <累計バイト数>`
   - 書き込みに失敗したら `OTA ERR write`
   - 10 秒バイトが来なければ `OTA ERR timeout`
   - どちらもスロットは切り替えず、行モードへ戻る (続けて次の `OTA SERIAL` を送ってよい)
5. 端末: `size` バイトを受け切ったら esp_image を検証する
   - 成功: NVS に「確定待ち」の印を立て、`OTA OK` を返し、約 500 ms 後に再起動する
   - 失敗: `OTA ERR verify` (スロットは切り替えない)
6. 再起動後: ホストは再接続して `DEVICE` を送る → `DEVICE timecard VER=<ver> FLAVOR=timecard-station`
   (CoreS3 は `DEVICE cores3 VER=<ver> BOARD=<board> FLAVOR=cores3` — `FLAVOR=` は `KEY=` で拾う。
   警告デバイスは `DEVICE alarm VER=<ver> FLAVOR=alarm`)
7. ホスト: FLAVOR が期待どおりなら `OTA CONFIRM` を送る
8. 端末: 確定待ちなら確定して印を消し、`OTA CONFIRMED RX=<受信リングのバイト数>` を返す
   (ホストは `OTA CONFIRMED` で始まる行として待つ。`RX=` の無い古い版も在る)
   - 確定待ちでなくても同じ行を返す (冪等)
   - 印があるのに起動から 10 分 (`OTA_VERIFY_TIMEOUT_MS`) 以内に `OTA CONFIRM` が
     来なければ、前の image へ戻す。戻った先の起動で
     `EVT OTA_ROLLED_BACK … reason=serial_unconfirmed` が出る
   - 起動時に確定待ちなら `EVT OTA_SERIAL_PENDING slot=<label> timeout_ms=600000` が出る

- 版 (`VER`) の一致は「更新するか」の判定にだけ使う。確定の条件にはしない
- 印の無い未確定 (web インストーラ / `espflash` で入れた機) には何もしない
- `READY` の後のバイトは行に分けずに受けるので、途中に `\r` / `\n` があってもよい
  (受信は #277 で無変換)。CoreS3 は受信中だけ Improv フレームの振り分けも止める
  (イメージの中身が IMPROV マジックに当たりうる)。受け切る・`OTA ERR write|timeout|verify`
  のどれで終わっても、次のバイトから行と Improv の振り分けに戻る
- **受信中はホストが他の行を送れない** (`HB OK` も生バイトとして読まれる)。CoreS3 の
  沈黙警告は heartbeat が 10 秒途切れると鳴るので、ホストは `OTA SERIAL` の前に
  `HB OFF` で監視を止める (または `HB OK grace=<秒>` で締切を延ばす) こと
- **警告デバイス (`alarm`) と、警告デバイスを兼ねる Vein Base (`timecard` の `vein` build) も同じ**: ホストは `OTA SERIAL` の前に heartbeat の見張りを
  休ませる (`HB OFF` か `HB OK grace=<秒>`)。休ませないと、受信中は `HB OK` が届かず
  10 秒でブザーが鳴る。警告デバイスは `ws_uplink` を持たないので、確定の口は
  `OTA CONFIRM` だけ (来なければ 10 分で前の image へ戻る)
- WS の常時接続 (`ws_uplink`) も、繋がった時点 (未登録の機は起動直後) で image を
  確定する (Refs #217)。LAN / Wi-Fi のある CoreS3 では `OTA CONFIRM` より先にそちらで
  確定することがあるが、そのあとの `OTA CONFIRM` にも `OTA CONFIRMED` を返す (冪等)
- ホストは FLAVOR に対応する bin を送る (機体は flavor が自分と完全一致しなければ
  `OTA ERR flavor` で断る)。Pages の app 単体イメージ: `cores3` =
  `firmware/alc-hub-cores3-app.bin`、`cores3-wifi` = `firmware/alc-hub-cores3-wifi-app.bin`、
  `cores3-dev` = `firmware/alc-hub-cores3-dev-app.bin`、`timecard-station` =
  `firmware/alc-hub-atoms3-timecard-station-app.bin`、`alarm` =
  `firmware/alc-hub-atoms3-alarm-app.bin`。版は同じ階層の manifest
  (`manifest.json` / `manifest-wifi.json` / `manifest-timecard-station.json` /
  `manifest-alarm.json`) の `version`

**既知の限界**: 新しい app が起動直後に落ちる (確定の口までたどり着かない) と戻らない —
web インストーラの bootloader (espflash 同梱の `boot.bin`) は rollback を持たず、
確定も戻しも app が行うため。USB はつながっているので、web インストーラで焼き直して
復旧する。
