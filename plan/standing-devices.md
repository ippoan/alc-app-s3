# 常設デバイス 2 台 — NFC タイムカード端末 / 点呼端末の警告デバイス

CoreS3 統合ハブ (ルートの `alc-hub-cores3`) と `hub-*` クレート群を共有する
**別バイナリ 2 本**の設計。既存の `crates/atoms3-print` (印刷ブリッジ) /
`crates/atoms3-nfc` (NFC ベンチ) と同じ位置づけで、ワークスペースに crate を
足し、CI に build leg を足し、Pages にインストーラを足す。

2 台を 1 本の doc にまとめたのは、**共有部分が設計の大半を占める**ため:
`speaker.rs` のボード非依存化、device role / kind の追加、build.yml の leg、
Pages インストーラ、provisioning はどちらにも同じ形で効く。装置固有の話は
§3 (タイムカード) と §4 (警告デバイス) に閉じてある。

- 音の設計 (§2) は **2026-09-04 に旧 ATOM Voice 実機で測った結果**が根拠。
- 既存資産のパス・シンボルは **2026-09-04 に repo を読んで実在を確認済み**
  (行番号は当時のもの)。

## 0. 結論サマリ

| | (1) NFC タイムカード端末 | (2) 点呼端末の警告デバイス |
|---|---|---|
| 用途 | 営業所常設。NFC タップ → 打刻音 → 打刻イベント送信 | キオスク異常 (WebSerial/ブリッジ切断) と点呼呼び出しを音で知らせる |
| ハード | Atom VoiceS3R (C126-ECHO) + Atomic PoE Base (A091) + Unit NFC (U216) | ATOM 系 + USB でキオスク PC に接続 (給電も USB) |
| 通信 | PoE 1 本 (W5500) + 既存 WS 常時接続 | Wi-Fi + 既存 WS 常時接続 (下り command) / USB は給電と heartbeat |
| 新 crate | `crates/atoms3-timecard` (仮) | `crates/atoms3-alarm` (仮) |
| 雛形 | `crates/atoms3-print/src/main.rs` | 同上 (LAN を Wi-Fi に差し替え) |
| 新 device role | `device-timecard` | `device-alarm` |
| 送信 | `kind: "timecard"` を既存 WS uplink に載せる | 送信なし (下り受けのみ)。crash_log は載る |

### 設計の要点 (先に結論だけ)

1. **`card_id` に接頭辞を付けない** — `felica:` / `license:` を足すと既存 2 経路が
   必ず外れる (§3.2)。**生値のまま**送り、種別は別フィールド `card_kind` で運ぶ。
2. **打刻の判定 (出勤/退勤) は端末でやらない** — 端末は「誰が・いつ・どの端末で」だけ。
3. **HTTP 直行は採らない** — 既存 WS + `kind` 追加で済む。tenant は端末の申告ではなく
   **device JWT の introspect 結果**から解決する (これは既存経路がそう作られている)。
4. **警告デバイスは「命令で鳴る」のではなく「沈黙で鳴る」** — ブラウザが定期的に
   「正常」を送り、途切れたら端末が自分の判断で鳴る (§4.1)。
5. **`speaker.rs` はボード非依存部分だけを残して共有する** — ES8311 は新規実装 (§2.3)。

## 1. 前提 — なぜ専用ハードを起こすのか

**既にブラウザ版の打刻端末がある**。この doc の (1) は 3 世代目ではない。

- `alc-app` の `web/app/components/TimePunchKiosk.vue` (185 行) — ローカル NFC
  ブリッジ (`ws://127.0.0.1:9876`, `useNfcWebSocket.ts`) の `nfc_read` を受けて
  `punchTimecard(cardId, deviceId)` を叩き、当日一覧を表示する。
  運用手順は `alc-app` の `docs/operator/timecard.md` (「開発中」と明記)。

専用ハードを起こす理由は次の 4 点に限る。これ以外の機能は**足さない**:

| 理由 | ブラウザ版で満たせない点 |
|---|---|
| **常設** | ブラウザ版はタブを開いている間だけ。閉じる/スリープで打刻できない |
| **PC 不要** | 打刻専用に Windows PC + NFC ブリッジ (常駐アプリ) を 1 台占有する必要がなくなる |
| **PoE 1 本で設置** | 電源コンセントの無い場所 (出入口の壁) に置ける。LAN 1 本で給電も通信も済む |
| **打刻音** | ブラウザ版は PC スピーカー依存で、無音 PC / ミュートで無反応に見える |

**ブラウザ版は撤去しない**。タブレットしか無い拠点や、専用機の故障時のフォール
バックとして残す。両者が同じ `POST /api/timecard/punch` に着地する形を保つこと
(= §3.3 で「打刻の実体は rust-alc-api 側の 1 か所」に寄せる理由)。

### 1.1 CoreS3 で作る案 (比較対象)

`speaker.rs` の doc コメントに重要な更新がある —
**I2S DOUT=G13 は旧 LAN Module 13.2 の CS と同一ピンで排他だったが、Base LAN
PoE v1.2 (CS=G9) では競合しない**ため `lan` feature と同時に有効化できる。
つまり **CoreS3 + Base LAN PoE + Unit NFC + 内蔵スピーカー**は既に成立する構成で、
新規のオーディオ実装 (§2.3) が丸ごと要らない。トレードオフは単価と筐体サイズ:
CoreS3 は Atom より一回り高く、壁付けの打刻端末には過剰な 2 インチ LCD を持つ。
**打刻端末は「壁に貼る小さい箱」であることが要件**なので Atom 系を採る。ただし
ES8311 の実装 (§2.3) が想定より深く刺さった場合、**CoreS3 で作れば音は今日動く**
という退路があることは記録しておく。

## 2. 音の設計 (2026-09-04 実機測定で確定)

旧 ATOM Voice (ESP32, NS4168) の実機テスト結果:

- CoreS3 の音声素材 (`crates/hub-drivers/assets/touroku_kanryo_24k_s16le.raw`、
  24kHz mono s16le、ピーク -8.3 dBFS / RMS -23.5 dBFS) は**そのままでは全く聞こえない**
- tanh 圧縮で RMS -5.4 dBFS まで上げてやっと聞こえるレベル。**圧縮を倍
  (g=6→14) にしても +2.3 dB しか増えない = デジタル側の天井**
- 同じ最大振幅で 1000/1500/2000/2500/3000/4000/5000 Hz を鳴らし比べ
  **3000Hz が最大**。2600〜3400 の細分でも 3000 が頂点 (小型スピーカーの共振帯域)
- **単独の音声案内は不採用**。ただし**ビープで注意を引いた後の音声メッセージは
  成立する** (実機で確認済み)

### 2.1 鳴らし分け (確定)

| 場面 | 音 | 機 |
|---|---|---|
| 打刻成功 | 3000Hz 60ms ×2 (間隔 40ms) — 速く 2 回 | (1) |
| 警告 | 3000Hz 200ms ×3 → 音声メッセージ → 1.2 秒 → **止めるまで繰り返す** | (2) |
| 停止 | **本体ボタン**、または状態の解消 (点呼開始・再接続) | (2) |
| 停止の合図 | 1200Hz 150ms 1 発 (「直った」のか「人が止めた」のか区別するため) | (2) |

「停止の合図」を分けるのは**運用上の識別のため**: ボタンで黙らせたなら異常は
続いているので人が見に行く必要があり、状態解消で止まったなら放置でよい。
同じ音だとこの区別が現場でつかない。

### 2.2 音源の前処理

音源は小型スピーカー向けに前処理してから crate の `assets/` に置く:
**400Hz ハイパス + 1800Hz 以上の中高域強調 + tanh 圧縮**。低域は 0.5W では再生
できず電力を食うだけなので捨てる。前処理は**ビルド時ではなくオフラインで 1 回**
行い、加工済み `.raw` をコミットする (再現手順は crate の `assets/README.md` に
ffmpeg / sox のコマンド列として残すこと)。CoreS3 用の既存 raw は**そのまま残す**
— CoreS3 の AW88298 + 内蔵スピーカーでは無加工が最良と 2026-07-21 に実測済みで、
共有すると CoreS3 側が劣化する。

### 2.3 `speaker.rs` の分割 (両機で共有する部分)

現状の `crates/hub-drivers/src/speaker.rs` は CoreS3 専用の初期化と汎用の再生
ロジックが 1 ファイルに同居している。ボード依存は次の 2 点だけ:

| | 依存 | 内容 |
|---|---|---|
| ボード依存 | `init_amp` | AW88298 (I2C 0x36) + AW9523 (0x58 P0 bit2) の初期化 |
| ボード依存 | I2S ピン | `Speaker::new(i2s1, bck, ws, dout)` の実引数 |
| **共有可** | `Sound` enum / `start_player` | 再生スレッド分離 (I2S write がブロッキングなので必須) |
| **共有可** | `beep` / `play_pcm_24k_mono` / `feed_silence` | 矩形波生成・24k→48k 補間・フェード |

したがって:

1. `speaker.rs` を **codec 初期化 trait (もしくは `#[cfg]` 分岐) + 共通再生**に割る。
   `Speaker::new` は既にピンを引数で受けているのでそのまま使える。
2. **`Sound` enum を拡張する** — 現状 `BeepOk` / `Registered` の 2 つで、
   `start_player` の `match` にハードコードされている。§2.1 の 4 パターン
   (`PunchOk` / `AlertLoop` / `AlertStop`) を足す。ループ音は
   「止めるまで繰り返す」ので、`start_player` に**中断可能なループ**の口
   (`Sound::Stop` を受けたら再生中のループを抜ける) が要る — 現状の
   `while let Ok(sound) = rx.recv()` は再生中に次を受け取れないので、
   `recv_timeout` ベースへ書き換える。
3. **`speaker` モジュールの feature ゲートを直す**。今は
   `#[cfg(feature = "nfc-verify")] pub mod speaker;` (`hub-drivers/src/lib.rs`)
   になっており、**NFC と無関係に音だけ欲しい機 (2) がビルドできない**。
   `speaker` feature を新設し、`nfc-verify` からは切り離すこと。

**ES8311 + NFC の初期化は新規に書く必要がある**。2026-09-04 の意味検索で
ippoan の公開 repo 全体に **ES8311 の実装は存在しない**ことを確認済み
(AW88298 = CoreS3 のみ)。参考実装は M5Unified / ESPHome の EchoS3R 設定。
CoreS3 で踏んだ罠 (issue #102) は ES8311 でもそのまま効く見込みなので、
**移植時に必ず持って行くこと**:

- サンプルレートは **48kHz 固定**。44.1kHz は ESP32-S3 の分数分周のジッタで
  codec の PLL がロックせず完全無音になる
- `Config::auto_clear(true)` 必須。false だと DMA がアンダーラン後に最後の
  バッファを送り続け、ビープが永久に止まらない
- **クロックを流してから codec を初期化する** (`feed_silence` → `init_amp`)。
  新 I2S ドライバは FIFO 空で BCK を止めるので、`tx_enable()` だけでは PLL が
  ロックしない
- 定数長の `Vec::resize` / 畳み込まれる定数は xtensa LLVM の ISel を踏む。
  `core::hint::black_box` で防いである箇所はそのまま持って行く

## 3. (1) NFC タイムカード端末

### 3.1 ハード構成とピン

**Atom VoiceS3R** (別名 Atom EchoS3R, M5 SKU C126-ECHO、ESP32-S3-PICO-1-N8R8 /
8MB Flash / 8MB PSRAM) + **Atomic PoE Base** (A091, W5500) + **M5 Unit NFC**
(U216, ST25R3916)。

ピンが競合しない根拠 (M5 公式 docs):

| 用途 | GPIO |
|---|---|
| 内蔵オーディオ (ES8311 codec / NS4150B アンプ / MEMS マイク) | G45(SDA) / G0(SCL) / G48(DOUT) / G4(DIN) / G3(WS) / G17(BCLK) / G11(MCLK) / G18(NS4150_CTR) — **すべて内部ピン** |
| Grove (HY2.0-4P) | G1 / G2 → Unit NFC (`atoms3-nfc` と同じ配線) |
| 底面バス | G5/G6/G7/G8/G38/G39 → PoE Base の W5500 SPI (`atoms3-print` 実績: SCLK=G5 / MISO=G7 / MOSI=G8 / CS=G6) |

#### 旧 ATOM Voice (ESP32-PICO-D4, C008-C) を選ばない理由

内蔵オーディオが**底面バスの** G19/G22/G23/G33 を占有し、M5 公式が「拡張
モジュールでの再利用は禁止」と明記している。ATOM (ESP32) 系の底面バスは 6 本
しかないので 4 線 SPI の PoE ベースが載らない。加えて非 S3 でビルドターゲットが
増える。S3R はオーディオが内部ピンに移り、ターゲットも `xtensa-esp32s3-espidf`
のままなので両方解消する。

#### 未確認 — **発注前に回路図 PDF で底面バスを確認すること**

1. VoiceS3R 固有ページに底面バスのピン一覧が無く、**AtomS3R のピンマップからの推定**
2. スイッチサイエンスの PoE ベース対応表は AtomS3 / AtomS3 Lite までで S3R 系が
   未記載 (M5 docs 側には AtomS3R とのバス接続表あり)
3. **PSRAM 8MB 搭載**のため、PSRAM 非搭載の AtomS3 (`atoms3-print`) 向け
   `sdkconfig.defaults` は流用不可。ルートの CoreS3 用 SPIRAM 設定
   (`CONFIG_SPIRAM_MODE_QUAD` 等) を参照して S3R 向けに起こし直す。
   **CoreS3 の QUAD/OCT で 1 度踏んでいる** (OCT 指定で "PSRAM chip is not
   connected" → `IGNORE_NOTFOUND` により黙って PSRAM なし起動、`EVT HEAP` の
   `free_psram=0` で発覚) ので、起動ログの `free_psram` を必ず確認すること

### 3.2 `card_id` の扱い — 接頭辞を付けてはいけない

当初 `felica:` / `license:` の名前空間を考えたが、**既存 2 経路を壊す**ことが
判明した:

- `rust-alc-api` の punch は `find_card_by_card_id` が外れると
  **`employees.nfc_id` (免許証 16 桁の生値) にフォールバック**する
  (`crates/alc-misc/src/timecard.rs:146,155`)
- alc-app の登録 UI も生値をそのまま入れる
  (`web/app/components/TimecardManager.vue:55` の `createTimecardCard`)

照合は `SELECT * FROM timecard_cards WHERE tenant_id = $1 AND card_id = $2`
(`crates/alc-misc/src/repo/timecard.rs:120`) の**完全一致**なので、prefix を
付ければ両方が必ず外れる。

**決定: `card_id` は生値のまま送る。種別は `payload.card_kind` という別フィールドで運ぶ。**

```json
{ "card_id": "0123456789ABCDEF", "card_kind": "felica_idm" }
{ "card_id": "1234567890123456", "card_kind": "license" }
```

`card_kind` は当面 **記録と診断のためだけ**に使い、照合には使わない
(照合に使い始めた瞬間、ブラウザ版が送る `card_kind` 無しの punch と挙動が割れる)。

#### 残課題 A: IDm と免許証番号の衝突

IDm は 8 バイトを `%02X` で 16 文字にした文字列
(`components/M5Unit-NFC/src/nfc/f/nfcf.cpp:69`、**大文字・区切りなし**)。
免許証番号は 16 桁の数字。**IDm の全ニブルが 0-9 になると両者は同じ文字列空間に
入る**。確率は (10/16)^16 ≒ 0.055% (約 1800 枚に 1 枚) で、**さらにその文字列が
実在の免許証番号として登録済み**である必要があるため実害はきわめて小さいが、
ゼロではない。緩和は次の順で:

1. **今は何もしない** — `card_kind` を記録しておき、衝突が起きたら
   `hub_measurements` の行から特定できる状態にしておく
2. 将来 fallback を絞るなら、`card_kind` が `felica_idm` のときは
   `employees.nfc_id` フォールバックを**行わない**、という条件を punch に足す。
   ブラウザ版は `card_kind` を送らないので、**送られてきたときだけ効く**追加条件に
   すれば後方互換が壊れない

#### 残課題 B: 大文字/小文字の揺れ (**実装前に必ず実測すること**)

照合は case-sensitive。本端末は **大文字**の IDm を送る。一方、ブラウザ版が
使うローカル NFC ブリッジ (Windows 常駐アプリ) が送る文字列の形式は
この repo からは分からない (`useNfcWebSocket.ts` の `nfc_read` イベントは
`employee_id` という名前のフィールドを持ち、`TimePunchKiosk.vue:69` が
それをそのまま `cardId` として punch に渡している — **フィールド名が実態と
ずれている**)。

**着手前のタスク**: 本番の `timecard_cards.card_id` を実際に引いて、
既存登録が大文字か小文字か、区切り文字があるかを確認する。結果次第で

- 既存が大文字 → 端末はそのまま送る (追加作業なし)
- 既存が小文字/混在 → **punch の入口で正規化する**
  (`card_id.trim().to_uppercase()` を照合の前に 1 回、`timecard_cards` 側にも
  同じ正規化を掛けた列 or 移行 UPDATE)。この場合は `rust-alc-api` の作業が増える

これを決めずに端末を作ると「タップしても『登録されていません』」で止まる。

### 3.3 打刻イベントの送り方

**端末は「誰が・いつ・どの端末で」だけ送り、出勤/退勤の判定はしない**
(front で処理する方針が決定済み)。

上り経路は**既存の WS uplink をそのまま使う**。`crates/hub-core/src/uplink.rs` の
`push_record` (seq 冪等 + NVS 永続の送信キュー、`MAX_QUEUE = 20`) に
`kind = "timecard"` で積むだけでよい。オフライン打刻の取りこぼし対策はこれで済む
(LAN 断・サーバ断の間はキューに溜まり、復帰後に同じ seq で再送、サーバ側
`UNIQUE (tenant_id, device_id, seq)` で冪等)。

```
{"type":"measurement","seq":N,"recorded_at_ms":T,"kind":"timecard",
 "payload":{"card_id":"0123456789ABCDEF","card_kind":"felica_idm"}}
```

`session_id` は**付けない** (点呼のセッションではないため。`uplink.rs` は
`None` なら key ごと省く)。

#### HTTP 直行案を採らない理由

端末から `rust-alc-api` の `POST /api/timecard/punch` を直接叩く案は、
**tenant 資格情報 (tenant JWT) を端末へ配布する必要が生じる**だけで利点がない。
punch は `tenant_router()` + `Extension<TenantId>` 前提
(`crates/alc-misc/src/timecard.rs:17-27`) で、device JWT では通らない。
既存 WS 経路なら tenant は **device JWT の introspect 結果**から
cf-alc-recorder が解決し (`cf-alc-recorder/src/index.ts` の `authenticateDevice`
→ `decideRecorderAuth` → `X-Recorder-Tenant-Id`)、端末は tenant を名乗らない。
**端末の申告する tenant を信じる経路を新設しない**、が既存設計の不変条件。

#### 中継の実装場所 — recorder 側か rust-alc-api 側か

**推奨: rust-alc-api の `hub_measurements::ingest` の中で punch する。**

現在の流れは `端末 --WS--> cf-alc-recorder --(内部proxy)--> rust-alc-api
POST /api/hub/measurements`。ここで:

- **cf-alc-recorder は `kind` を素通しする**。`cf-alc-recorder/src/measurements.ts`
  は `kind` を「トップレベル優先、無ければ `payload.type`」で拾い、
  **非空チェックだけ**で allowlist を持たない (allowlist は rust-alc-api 側)。
  つまり **`kind: "timecard"` は Worker を無改造で通り抜ける**。
- 内部 ingest は `require_internal_shared_secret` middleware が `X-Tenant-ID` から
  `TenantId` extension を挿す (`crates/alc-core/src/auth_middleware.rs:128,145`)。
  **punch ハンドラが要求する `Extension<TenantId>` と同じ型**なので、tenant の
  受け渡しに新しい仕掛けが要らない。
- 冪等が既にある。`insert_batch` は 1 件ずつ
  `ON CONFLICT (tenant_id, device_id, seq) DO NOTHING` で入れ、
  `res.rows_affected()` を見ている
  (`crates/alc-devices/src/repo/hub_measurements.rs:43,55`)。
  **「新規に入った行のときだけ punch する」**と書けば、端末の再送で
  二重打刻にならない。これは recorder 側で中継する設計では自前で作り直す必要がある。

対して **cf-alc-recorder で中継する案**は、(a) punch が tenant JWT 専用なので
どのみち rust-alc-api に内部ルートを新設する必要があり、(b) ネットワーク往復が
1 つ増え、(c) 「測定行は入ったが punch は失敗」という部分成功状態を新たに作る。
利点が見つからない。

**したがって cf-alc-recorder のコード変更は不要**と見込む (§5 の一覧では
「変更不要の確認」タスクとして残す — `kind` 素通しは読んだ限りの結論なので、
staging で 1 回実際に通して確かめること)。

#### `device_id` の型が合わない (要設計)

`time_punches.device_id` は **UUID** で `alc_api.devices(id)` への FK
(`migrations/036_add_device_id_to_time_punches.sql`)。一方 hub 側の `device_id` は
auth-worker が発行する **URL-safe な短い文字列** (`hub_measurements.device_id` は
文字列カラム、上限 128)。そのままでは入らない。選択肢:

1. **`device_id = NULL` で punch する** (最小)。`hub_measurements` 側に device_id が
   残るので「どの端末か」は追える。`device_id` は nullable なので変更不要。**推奨**
2. `devices` テーブルに端末を 1 行作り、hub device_id → UUID の対応表を持つ

まず 1 で出し、運用で「打刻機ごとの集計」が要るとなってから 2 を検討する。

### 3.4 他 repo の作業 (この方針での実際の差分)

| repo | ファイル | 作業 |
|---|---|---|
| `rust-alc-api` | `crates/alc-devices/src/hub_measurements.rs:52` | `HUB_MEASUREMENT_KINDS` に `"timecard"` を足す。**「将来の拡張 (timecard イベント等) はここに足す」と doc コメントに先に書かれている**箇所 |
| `rust-alc-api` | 同 `ingest` | 新規行のときだけ punch を呼ぶ。punch 本体 (`crates/alc-misc/src/timecard.rs` の `punch`) からカード照合部分を関数に括り出して再利用する (二重実装しない) |
| `rust-alc-api` | `coverage_100.toml:225` | `crates/alc-misc/src/timecard.rs` は**登録済み** = 分岐ごとのテストが必須。カード有り / nfc_id フォールバック / 未登録 / 重複 seq の 4 分岐を足す |
| `alc-app` | `web/app/types/index.ts:1159` | `HUB_MEASUREMENT_KINDS` (現在 5 種) に `'timecard'` を追加。backend の同名 allowlist と一致させる |
| `alc-app` | `cf-alc-recorder` | **変更不要の見込み**。staging で `kind: "timecard"` が素通ることを 1 回確認する |
| `auth-worker` | `src/lib/device.ts:139` | `DEVICE_ROLE_TIMECARD = "device-timecard"` を定義し `DEVICE_ROLES` に追加 |
| `auth-worker` | `src/handlers/device-setup.ts:96` | `DEVICE_KINDS` に `timecard` エントリ (role / appUrl / manifestUrl / installerUrl / display)。**role と kind は 1:1** に保つ (CoreS3 firmware を打刻機へ push する取り違えを構造的に防ぐ既存の設計) |
| `alc-app` | `cf-alc-recorder/src/auth.ts:46` | `RECORDER_DEVICE_ROLES` に新 role を追加 (これが無いと WS が 403) |

## 4. (2) 点呼端末の警告デバイス

点呼キオスク (ブラウザ) の異常を人に気付かせる据置ブザー。用途は 2 つ:

- **キオスクの入力デバイスが切れた** — FC-1200 (WebSerial,
  `alc-app/web/app/composables/useFc1200Serial.ts`) / NFC ブリッジ
  (localhost WebSocket `ws://127.0.0.1:9876`, `useNfcWebSocket.ts`)。
  **この 2 つは別々の transport** (前者が WebSerial、後者はローカル WS 常駐アプリ)
  だが、警告デバイスから見れば「キオスクが正常に測定できる状態か」の 1 ビットで足りる
- **点呼の呼び出しが来ている**

### 4.1 検知の向き — 沈黙を異常とみなす (設計の要)

**ブラウザが「鳴れ」と命令する形にしない。ブラウザが定期的に「正常」を送り続け、
途切れたら端末が自分の判断で鳴る。**

命令駆動だとブラウザ/PC が落ちたときに命令が来ず沈黙する = **一番危ないケースで
鳴らない**。沈黙を異常とみなせば、ブラウザのクラッシュ・タブを閉じた・PC の
フリーズを**同じ形で**拾える。

```
ブラウザ --(USB CDC, 数秒ごと)--> ATOM   "OK <flags>"
                                    ↓ 一定時間 受信なし
                                  鳴る (§2.1 の「警告」)
```

heartbeat の中身は最小限にする。**キオスクが自分で判断した結果** (正常か否か) を
1 行で送り、端末側に判断ロジックを置かない:

```
HB OK            … 正常 (FC-1200 も NFC ブリッジも生きている)
HB NG serial     … キオスクが異常を自覚している (理由は表示用のラベル)
```

`hub-core/src/protocol.rs` の `parse_line` が既に行指向のホストコマンドを解釈して
いるので、`HB` を 1 コマンド足す形にすれば端末側の実装は既存の `console.rs`
(`atoms3-print/src/console.rs`、209 行) の縮小版で済む。

タイムアウト値は**運用で決める**が、初期値は heartbeat 3 秒 / 無音 10 秒で鳴る
あたりから始める (ブラウザの `setInterval` はタブが背面だと 1 秒までスロットル
されるので、3 秒間隔なら背面でも間に合う)。

#### 許容する穴

**USB 給電なので PC の電源が落ちるとブザーも死ぬ**。ユーザー判断で**許容**
(人が見れば分かる異常のため)。PoE / AC アダプタで独立給電にすればこの穴は
塞がるが、その場合は「PC が落ちた」と「USB ケーブルが抜けた」の区別が付かなく
なるので、**穴を塞ぐより穴があることを運用に伝える方が安い**という判断。

### 4.2 「点呼が来ている」の経路

`cf-alc-recorder` の既存の下り push (`{"type":"command", ...}`) をそのまま使う。
`hub-drivers/src/ws_uplink.rs` が device JWT で WS を張りっぱなしにして下り
command を受ける実装が既にある (`handle_downlink` の `command_action` 分岐、
未知の action も空 result で ack する作り)。認証は
`cf-alc-recorder/src/index.ts` の `authenticateDevice` (device JWT introspect) が
そのまま効く。push の入口も既存: `POST /tenants/:tenantId/devices/:deviceId/command`
(`cf-alc-recorder/src/index.ts:237`)。

新 action を 1 つ足す (例 `action: "alert"` + `payload.state: "call" | "clear"`)。
端末側は `ws_uplink.rs` の match に 1 分岐を足すだけ。

#### **要設計: この機はどうやってネットワークに出るか**

ハード構成は「ATOM 系 + USB でキオスク PC に接続 (給電も USB)」なので、
**USB では IP に出られない**。WS で下り command を受けるには別の経路が要る:

| 案 | 内容 | 評価 |
|---|---|---|
| **A. Wi-Fi + WS** | ATOM の Wi-Fi で `cf-alc-recorder` に常時接続。provisioning は既存の Improv Wi-Fi Serial (`crates/hub-wifi` + `hub-core/src/improv.rs`) | **推奨**。キオスク PC が死んでいても点呼呼び出しは鳴る。既存資産をそのまま使える |
| B. heartbeat に相乗り | ブラウザが `HB OK call=1` のように呼び出し状態も載せる | 端末に IP もクレデンシャルも不要で**圧倒的に簡単**。ただし**ブラウザが死んでいると呼び出しが鳴らない** — 沈黙警告は鳴るので「何かおかしい」ことは伝わる |

**A を本命とし、B を第 1 マイルストーンに置く**。B は追加コストがほぼゼロで
(heartbeat に 1 フィールド足すだけ)、A の Wi-Fi provisioning が現地で詰まった
場合の退路にもなる。A に進むと device credential・Wi-Fi 設定・OTA が付いてくるので、
**B で「音とボタンと沈黙検知」を現地で確定させてから** A を積む。

### 4.3 実装時の要確認 — WebSerial と DTR/RTS

M5 の USB シリアルは **DTR/RTS が ESP32 の EN/IO0 に繋がっており、開き方に
よってはポートを開くたびにリセットが入る**。**実測では DTR/RTS を触らずに開けば
リセットされなかった**が、Chrome の WebSerial が `open()` 時に DTR をどう扱うかは
未確認。`setSignals({dataTerminalReady: false})` を明示する必要があるかもしれない。

現状の `alc-app` には **`setSignals` の呼び出しが 1 か所も無い**
(`useFc1200Serial.ts` / `useBleGateway.ts` とも `port.open(SERIAL_OPTIONS)` のみ)。
つまり「触っていない」状態が既に FC-1200 で動いているので、同じ開き方で始めて、
**実機で「開くたびにリセットが入るか」をログで確認する**のが最短。入るなら
`open()` 直後に `setSignals` で落とす。

なお ATOM 系は `CONFIG_ESP_CONSOLE_USB_SERIAL_JTAG=y` (ネイティブ USB) で
コンソールを出しており (`atoms3-print` / `atoms3-nfc` の sdkconfig)、この経路は
USB-UART ブリッジ (CH9102 等) の DTR/RTS 配線とは事情が違う。**実機で確かめる**。

### 4.4 停止ボタン

ATOM 系の本体ボタン (**GPIO 番号は機種ごとに違う。この repo には ATOM 系ボタンの
実績コードが無いので、M5 公式ピンマップで確認してから配線すること** — LED の
GPIO を Web 検索の要約だけで書いて実機で無点灯になり、M5Unified のボード定義を
読み直して G38→G35 と判明した実害が `atoms3-nfc` にある) を押したら鳴動を止め、
§2.1 の「停止の合図」(1200Hz 150ms) を鳴らす。
止めた事実は `EVT` としてホスト (キオスク) へも出し、ブラウザ側で「ブザーを人が
止めた」ことが分かるようにする。**異常が解消していない限り、次の周期でまた鳴らす
ようなことはしない** (人が認識済みなので)。異常が一度解消して再発したら、また鳴る。

## 5. 共有の作業 — ワークスペース / CI / インストーラ / provisioning

### 5.1 新 crate

`crates/atoms3-print` の構成をそのまま踏襲する:

- `Cargo.toml` に `[[bin]]` + `[package.metadata.esp-idf-sys]`
  (`esp_idf_sdkconfig_defaults` は crate 相対ではなく**ワークスペースルート基準**の
  文字列で書く)、`extra_components` は `esp_websocket_client` と
  (機 (1) のみ) `component_dirs = ["../../components"]`
- **`ESP_IDF_SYS_ROOT_CRATE=<crate 名>` を必ず指定してビルドする**
  (指定しないとワークスペース root package = CoreS3 の sdkconfig が使われる)
- ルート `Cargo.toml` の `[workspace] members` に追加
- `sdkconfig.defaults` は機ごとに新規。機 (1) は **PSRAM あり**なので
  `atoms3-print` のものを流用しない (§3.1 未確認 ③)
- `partitions.csv` は 8MB Flash なので `crates/atoms3-print/partitions.csv` を流用可
  (nvs=0x9000 / otadata=0x10000 は build.yml の Split step と結合しているので動かさない)
- 雛形は `crates/atoms3-print/src/main.rs` (144 行)。`crashlog::init` →
  `Settings::new` → `heap::start` → `console::start` → `ws_uplink::start` →
  (LAN or Wi-Fi) → `ota::mark_boot_valid` の順番はそのまま守る
  (**`crashlog::init` は `heap::start` より前**。配線漏れで `.noinit` のゴミ帳簿に
  書いて boot loop になった実害が 2026-07-14 にある)

機 (1) の NFC 読み取りループは `crates/atoms3-nfc/src/main.rs` (328 行) の
**存在検知ゲート (`PRESENCE_DELTA`) + F→A→B 逐次ポーリング**をそのまま持ってくる。
免許証 EF2F01 も同じループで読めているので、§3.2 の `card_kind` 出し分けは
「どの分岐で読めたか」で決まる。**新しい NFC コードは書かない**。

### 5.2 CI (`.github/workflows/build.yml`)

- `check` job の matrix に 2 leg 追加
  (`ESP_IDF_SYS_ROOT_CRATE=... cargo check --release --locked -p ...`)。
  `cache_key` は **sdkconfig が違うので新規** (`build-timecard` / `build-alarm`)。
  「全 leg 一律 `build` だと別フレーバーの esp-idf-sys 成果物が永遠に cold」
  という #63 の実測があるので、ここを揃えてはいけない
- `build` job の matrix に 2 leg 追加 (`bin` / `build_env` / `flash_size: 8mb` /
  `partition_table` / `cache_key`)
- `assemble-pages` の cp 行と manifest 生成を追加 (build.yml:528-538 付近)

**dev チャネルは出さない。** 現状 `manifest-dev.json` は cores3 のみで、
dev バリアントの実体は `mem-hud` feature (hub-ui のメモリ HUD) — 画面を持たない
Atom 系には意味がない。`assemble-pages` の出力は **3 本 → 5 本** になる
(manifest / manifest-dev / manifest-atoms3-print / manifest-timecard / manifest-alarm)。

### 5.3 Pages インストーラ

`docs/atoms3-print.html` (112 行) + `docs/manifest-atoms3-print.json` を雛形に
機種ごとに 1 組ずつ足す。manifest の `parts` は **boot.bin(offset 0) +
main.bin(offset 65536) の 2 分割**を必ず守る (merged を offset 0 に一括で書くと
0xFF パディングが NVS を潰し、再インストールのたびに device credential が飛ぶ、
Refs #48)。`docs/index.html` から相互リンクを張ること。

### 5.4 provisioning

**既存方式を踏襲し、新しい仕掛けを作らない。** `hub-common/src/settings.rs` の
NVS キー (`dev_id` / `dev_secret` / `dev_tenant` / `auth_url` / `ws_url`) と
`host_link.rs` の `AUTH SET <id> <secret> <tenant>` / `AUTH URL` / `WS URL`
コマンドがそのまま使える (`atoms3-print/src/console.rs` が既に縮小版を持っている)。

操作は auth-worker の `/device/setup` ページから WebSerial で行う
(**シリアルを人手で叩かせる手順にしない**)。ページ側は §3.4 の
`DEVICE_KINDS` エントリを足せば機種が増える。

## 6. マイルストーン

**ファームウェアの実装は本 doc のスコープ外**。以下は issue に分割する単位。

### (1) NFC タイムカード端末

0. **発注前**: VoiceS3R の回路図 PDF で底面バスを確認 (§3.1)。
   本番 `timecard_cards.card_id` の大文字/小文字を実測 (§3.2 残課題 B)
1. `speaker` の feature 分離 + `Sound` 拡張 + ES8311 初期化 (§2.3)。
   **実機で 3000Hz が鳴るまでが 1 マイルストーン**
2. crate 雛形 + PoE リンクアップ (`atoms3-print` の Milestone 0 と同形)
3. NFC ループ移植 + 打刻音 (ここまでネットワーク送信なし、シリアルログのみ)
4. `kind: "timecard"` の WS 送信 (端末側)
5. `rust-alc-api` 側の受け口 + テスト (§3.4)
6. CI leg + インストーラ + `/device/setup` の機種追加

### (2) 警告デバイス

1. `speaker` 共有 (機 (1) のマイルストーン 1 と共通 — **先行させる**)
2. crate 雛形 + heartbeat 受信 + 沈黙検知 + 鳴動 + 停止ボタン
   (**ネットワークなし・USB だけで完結**)
3. `alc-app` 側: キオスクから heartbeat を送る (§4.1)。
   §4.2 案 B (呼び出し状態を heartbeat に相乗り) までをここで出す
4. Wi-Fi + WS 常時接続 + 下り `alert` command (§4.2 案 A)
5. CI leg + インストーラ + `/device/setup` の機種追加

## 7. 参考 (実在を 2026-09-04 に確認)

- `crates/atoms3-nfc/src/main.rs` — AtomS3 Lite + Unit NFC の実績コード。
  存在検知 (アンテナ振幅 `PRESENCE_DELTA`) → 逐次ポーリングで**交通系 IDm と
  免許証 EF2F01 の両方**を読む。`components/nfc_shim` 経由の C++ FFI
- `crates/atoms3-print/src/main.rs` — AtomS3 + Atomic PoE Base の実績骨格。
  `eth_w5500` / `ota` / `ws_uplink` / `crashlog` / `heap` を結線済み。**新バイナリの雛形**
- `crates/hub-core/src/uplink.rs` — WS フレーム組立 + NVS 永続の送信キュー
  (seq 冪等、`push_record`)
- `crates/hub-drivers/src/ota.rs` — OTA と `mark_boot_valid` の rollback 安全装置
- `crates/hub-drivers/src/speaker.rs` — 音源・`Sound` enum・`start_player`
- `docs/index.html` + `docs/manifest.json` / `docs/atoms3-print.html` +
  `docs/manifest-atoms3-print.json` / `.github/workflows/build.yml` の
  `assemble-pages` job (build.yml:528-538)
- `plan/nfc-card-identity.md` — NFC 方式調査 (IDm / 免許証 / マイナンバー)
- `plan/cores3-hub-consolidation.md` — CoreS3 のモジュール配線一次情報
