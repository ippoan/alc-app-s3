# alc-fp001s-rp2354

FP001S(Grow の顔・手のひら静脈モジュール)端末のファーム。RP2354A(ippoan/vein-base の `pcb/fp001s_test_board` t3)か、同じ配線の Raspberry Pi Pico 2 で動く。

- USB は CDC 2 本(composite): **1 本目 = コンソール**(`docs/console-protocol.md`。`PING` / `DEVICE` / `AUTH KEYGEN [FORCE]` / `AUTH PUBKEY` / `AUTH SIGN <nonce>`)、**2 本目 = FP001S の UART そのまま**(115200 8N1、`EF AA` のフレームを両方向に素通し)。
- UART0: GP0 TX → FP001S の RXD(黄)、GP1 RX ← TXD(黒)。
- 鍵は VoiceS3R(`alarm`)と同じ: `AUTH KEYGEN` で機体が ed25519 の鍵対を作り、seed はフラッシュの最後の 4 KiB に置く(外へは出さない)。応答の形は `alc_hub_core::alarm_key` と 1 文字も違わない(no_std の写し、`src/proto.rs`)。Secure Boot と OTP は使わない(取り消せない操作は無い)。
- 次: シリアル OTA(`OTA SERIAL` / `OTA CONFIRM`、A/B と ROM の Try Before You Buy)。

## ビルドと書き込み

ルートの ESP のワークスペースとは別(stable + `thumbv8m.main-none-eabihf`、このディレクトリの `rust-toolchain.toml` / `.cargo/config.toml`)。

```sh
cd crates/fp001s-rp2354
cargo build --release
# BOOTSEL(Pico 2 はボタンを押しながら USB、t3 は TP1 と TP2 をショートして電源)で
picotool load -u -v -x -t elf target/thumbv8m.main-none-eabihf/release/alc-fp001s-rp2354
```
