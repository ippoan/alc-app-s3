# plan: WS push によるリモート印刷 (PDF を device token WS で AtomS3 へ)

Refs #38 (親: #37)。当初の #38 は「PDF を URL から HTTP GET → 9100」だったが、
URL を public にする必要 (CF Access / R2 / ローカル :9000 公開) が課題だった。
本プランは **device token で張った WebSocket に PDF 本体を push** する方式へ
設計を更新し、public URL を不要にする。

## 全体像

```
operator UI ─PDF POST─▶ alc-print-console (Cloudflare Worker + DO: PrintSessionDO)
(CF Access認証)              │  ①device token WS を device_id で保持
                            │  ②operator POST の PDF を該当 device WS へ chunk push
       device token WS ◀────┤  ③device token 検証 → auth-worker /auth/introspect に委譲
                            ▼
                    AtomS3 (atoms3-print) ──9100/LAN──▶ プリンター
```

- **recorder (cf-alc-recorder) 不使用**。atoms3-print は測定源が無い印刷専用機
  なので、現状 recorder に張っている「下り command 待受専用 WS」の接続先を
  alc-print-console へ丸ごと切り替える。
- **public URL 不要** — GET しない。認証済み WS 内を PDF が流れる。
- **R2 不要 / :9000 ローカル公開不要**。

## 設計の裏取り (調査済み)

- **auth-worker 完全無改修**。`/auth/introspect` (`src/handlers/auth-introspect.ts`)
  は device JWT も同じ `JWT_SECRET` (HS256) で検証でき、
  `{active, role:"device-print", tenant_id, sub:device_id}` を返す。
  alc-print-console はこれをサーバー間で叩くだけ。
- **印刷 DO は alc-print-console (別 worker)**。auth-worker は Release Wave の
  no-traffic 運用 (`release-wave.yml` + frontend-ci `release_no_traffic` default)
  で、新規 DO migration を足すと `error 10211`
  (`version_upload_migration_not_allowed`)。別 worker なら `wrangler deploy`
  (traffic) 運用にでき migration を流せる (`durable-object-worker` skill)。
- device JWT の role は pairing 時に確定する `DEVICE_ROLE_PRINT = "device-print"`
  (auth-worker `src/lib/device.ts`)。

## WS フレームプロトコル (既存 `Downlink::Command` を流用)

接続: device → `wss://<alc-print-console>/device/print/ws`、
`Authorization: Bearer <device-jwt>`、DO は `idFromName(device_id)` で 1 device
1 インスタンス。

下り (DO → device、既存 `{"type":"command","id","payload"}`):

| action | payload | device 動作 |
|---|---|---|
| `print_begin` | `{job, total}` | NVS の `printer_addr` へ `TcpStream` 接続、`PrintSession` 開始 |
| `print_data` | `{job, seq, chunk:"<base64>"}` | base64 decode → `TcpStream` write、`command_result` で ack |
| `print_end` | `{job}` | `flush`/close、`command_result` で `{phase:"done",bytes}` |

上り (device → DO、既存 `command_result_frame`): `{phase:"started|progress|done|error", ...}`。
DO は ack を待って次 chunk を送る (逐次)。WS 1MiB 上限内で 1 chunk = 32KB raw
/ 44KB base64 目安。

base64 は JSON 内に載せる (既存下りフレームが全て JSON テキストのため)。
firmware 側に手書き base64 decoder を hub-core に置き 100% coverage で担保する。

## repo 別タスク

### A. alc-print-console (新規・別セッション)

1. Worker scaffold: `wrangler.toml` (traffic `wrangler deploy` 運用,
   `new_sqlite_classes`)、CI (ci-workflows `frontend-ci.yml` project_type: worker)、
   `CLAUDE.md`、`README.md`。
2. `PrintSessionDO` (Hibernatable WebSocket、auth-worker `McpSession` を雛形):
   `/device/print/ws` accept → introspect で device token 検証 (role=device-print)
   → `idFromName(sub=device_id)`。
3. `POST /print/:deviceId` (operator, CF Access 保護): PDF 受領 → device WS へ
   print_begin / print_data (chunk) / print_end、`command_result` を進捗として返す。
4. operator UI ページ: PDF 選択 (履歴 localStorage) → POST、進捗表示。
5. CF Access: WS path は bypass (device は Bearer 認証、cookie を持てない)、
   UI は CF Access 保護。

### B. alc-app-s3 firmware (本セッション)

1. `crates/hub-core/src/uplink.rs`:
   `command_print_chunk(payload) -> Option<PrintChunk{seq, data:Vec<u8>, last}>`
   + 手書き base64 decoder (純粋関数) + tests。coverage_100.toml 対象。
2. `crates/hub-drivers/src/ws_uplink.rs`: `handle_downlink` に
   print_begin / print_data / print_end arm。`PrintSession{printer:TcpStream, job, bytes}`
   を `run` ループが `Option<PrintSession>` で保持し、フレーム跨ぎで生かす。
3. `crates/hub-drivers/src/printer.rs`: `TcpStream` 接続 / write / flush を
   `PrintSession` 用に切り出す (既存 `fetch_and_send` の URL-GET モデルから分離)。
4. `crates/hub-common/src/config.rs` / `settings.rs`: WS URL 既定を
   alc-print-console のエンドポイントへ (NVS `ws_url` キーは形式が同じなので流用可、
   `WS URL` コマンドで運用上書きも可)。

### C. auth-worker

**無改修**。`/auth/introspect` を alc-print-console が利用するのみ。

## セッション / PR 分担

| フェーズ | repo | セッション | 成果物 |
|---|---|---|---|
| 0 | alc-app-s3 | 本 | 本 `docs/plan-ws-print.md` + #38 設計追記 |
| 1 | alc-app-s3 | 本 | firmware 実装 (uplink/ws_uplink/printer/config) + tests |
| 2 | alc-print-console | 新 | DO worker + operator UI + CI/wrangler/CF Access |
| 3 | 実機 | ユーザー | AtomS3 フラッシュ → 印字 E2E |

## リスク / 未確定

- **実機 E2E は CCoW では不可** → ユーザーが AtomS3 で確認 (フェーズ3)。
- **CF Access 設定** (alc-print-console WS path bypass) は `cf-access-mcp` /
  dashboard。
- **firmware 先行の宙ぶらりん** — フェーズ1 を先に出しても繋ぐ DO (フェーズ2)
  は別セッション。プロトコルは本プランで確定済みなので両者は独立実装可能。
  E2E はフェーズ2完了後。
- **introspect のサーバー間認証** — 追加 shared secret が要るか実装時に確認
  (token 検証のみなので不要見込み)。

## 受け入れ基準

- firmware: `make test` (hub-core 100%) green。`print_data` で WS チャンクを
  9100 へ送信できる (unit level)。
- alc-print-console: typecheck + test green。operator が PDF 選択 → AtomS3 で印字。
- auth-worker: 差分ゼロ。
