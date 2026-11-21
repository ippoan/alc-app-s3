# alc-fp001s-key-ch32

FP001S(Grow の顔・手のひら静脈モジュール)端末 **t4**(ippoan/vein-base の `pcb/fp001s_test_board`)の、鍵だけを持つ CH32V203 のファーム。t4 では FP001S の UART は CH340C で PC へ直結し、この CH32V203 は認証の鍵だけを受け持つ(t3 の RP2354A 1 個の構成は `crates/fp001s-rp2354`)。

- USB CDC 1 本のコンソール(`docs/console-protocol.md`): `PING` → `PONG`、`DEVICE` → `DEVICE fp001s-key VER=<v> FLAVOR=fp001s-key`、`AUTH KEYGEN [FORCE]` / `AUTH PUBKEY` / `AUTH SIGN <nonce>`。ほかは `ERR UNSUPPORTED (fp001s-key)`。
- 応答の形は `alc_hub_core::alarm_key` と 1 文字も違わない(`crates/alarm-key-nostd`、fp001s-rp2354 と共有。同じ seed・nonce で hub-core と行を突き合わせるホストテストがある)。
- 認可: t3・alarm と同じく、届くのはローカルの USB だけで認可は無い(2026-11-18 ユーザー決定)。`AUTH SIGN` は小文字 hex 32 桁の nonce にしか署名しない(ほかは `ERR AUTH: bad nonce`)。鍵が在るときの `AUTH KEYGEN` は `FORCE` が無ければ `ERR AUTH: key exists`。
- 鍵の seed はフラッシュの最後の 256 B ページ(MAGIC `FPK1` + seed 32 B、fp001s-rp2354 と同じ形)。`memory.x` がコードをそのページへ置かない。読み出し保護・オプションバイトには触れない(取り消せない操作は無い)。全消去で書き直すと鍵は消えるので、`AUTH KEYGEN` → `AUTH PUBKEY` を登録し直す。
- シリアル OTA は持たない。更新は SWD(基板の SWDIO / SWCLK のテストポイント、WCH-LinkE)か ISP(BOOT0)。
- **実機は未確認**(基板が届いてから)。

## 型番: F6P6 ではなく 64 KiB の品

基板 t4 は CH32V203F6P6(flash 32 KiB / SRAM 10 KiB)で引いたが、ファームが収まらない(riscv32imac・`opt-level = "z"`・LTO の実測):

| ed25519 の実装 | ファームの大きさ |
|---|---|
| ed25519-dalek(hub-core と同じ) | 58 KiB |
| ed25519-compact(`opt_size`) | 66 KiB |
| salty(tweetnacl の体) | **41 KiB** ← 採用 |

salty でも USB(embassy-usb + ch32-hal の USBD)と初期化で約 17 KiB ある。なので **flash 64 KiB / SRAM 20 KiB で USBD が PA11 / PA12 の品**で build している(既定は `ch32v203f8u6`。G8R6 / K8T6 も同じファーム)。**F8P6 は不可**: TSSOP-20 で外形は F6P6 と同じだが、USB は USBFS で PB6 / PB7 に出る(ch32-metapac)。型番を変えるときは `Cargo.toml` の feature・`memory.x`・`src/key.rs` の `FLASH_SIZE` の 3 か所。

ed25519 は RFC 8032 で決定的なので、実装が違っても署名のバイトは同じ(ホストテストで確認)。salty は `slow-motion` で全機種とも tweetnacl の体を使う(付けないと Cortex-M33 の fp001s-rp2354 では別のアセンブリ実装を選び、ホストテストが通らない道になる)。

## 乱数(CH32V203 に TRNG は無い)

`AUTH KEYGEN` の seed は ADC の雑音から作る(`src/noise.rs` → `alarm_key_nostd::entropy`):

1. 内部温度センサと Vrefint を交互に、いちばん短いサンプル時間(1.5 サイクル、ADC クロック 12 MHz)で変換し、**最下位ビット**だけを 1 サンプルとする
2. 1 サンプルの min-entropy を **0.25 bit** と控えめに置き、**2048 サンプル**(512 bit 相当 = 出力 256 bit の 2 倍)を集める
3. 全サンプルに **SP 800-90B §4.4 の 2 検査**を掛ける(α = 2⁻²⁰):
   - Repetition Count Test: 同じ値が **81 回**続いたら不合格(C = 1 + ⌈20 / H⌉)
   - Adaptive Proportion Test: 1024 サンプルの窓で、窓の先頭と同じ値が **915 回**に達したら不合格(C = 1 + CRITBINOM(1024, 2⁻ᴴ, 1 − α))
4. 合格したら、詰めたビット列(256 B)の **SHA-256** を seed にする

どちらかの検査に落ちたら `ERR AUTH: entropy` で鍵を作らない(fail-closed)。生のサンプルを外へ出す口(デバッグ用の命令を含む)は作らない。閾値はホストテストが式から計算し直して照合する。実機の ADC の最下位ビットが 0.25 bit を下回るほど偏っていれば、検査に落ちて鍵が作れない形で表に出る。

## ビルドと書き込み

ルートの ESP のワークスペースとは別(stable + `riscv32imac-unknown-none-elf`、このディレクトリの `rust-toolchain.toml` / `.cargo/config.toml`)。

```sh
cd crates/fp001s-key-ch32
cargo build --release --locked
# SWD (WCH-LinkE) で
wlink flash target/riscv32imac-unknown-none-elf/release/alc-fp001s-key-ch32
```

- クロック: HSE 12 MHz の水晶 × 8 = 96 MHz、USB は 96 / 2 = 48 MHz(HSI は USB FS の精度に足りない)
- USB の VID/PID は pid.codes のテスト用 `1209:0001`(TODO: 正式な番号)。ホストは `DEVICE` の `<kind>` で見分ける
- ch32-hal は crates.io に無い(0.0.0 は placeholder)ので git の commit で固定。USBD ドライバが embassy の git の `embassy-usb-driver` を要るので、`embassy-usb` も同じ rev

共有 crate のホストテスト:

```sh
cd crates/alarm-key-nostd
cargo test --all-features --target x86_64-unknown-linux-gnu
```
