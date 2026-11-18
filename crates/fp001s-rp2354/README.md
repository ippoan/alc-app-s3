# alc-fp001s-rp2354

FP001S(Grow の顔・手のひら静脈モジュール)端末のファーム。RP2354A(ippoan/vein-base の `pcb/fp001s_test_board` t3)か、同じ配線の Raspberry Pi Pico 2 で動く。

- USB は CDC 2 本(composite): **1 本目 = コンソール**(`docs/console-protocol.md`。`PING` / `DEVICE` / `AUTH KEYGEN [FORCE]` / `AUTH PUBKEY` / `AUTH SIGN <nonce>` / `OTA SERIAL <size> fp001s` / `OTA CONFIRM`)、**2 本目 = FP001S の UART そのまま**(115200 8N1、`EF AA` のフレームを両方向に素通し)。
- UART0: GP0 TX → FP001S の RXD(黄)、GP1 RX ← TXD(黒)。
- 鍵は VoiceS3R(`alarm`)と同じ: `AUTH KEYGEN` で機体が ed25519 の鍵対を作り、seed はフラッシュの最後の 4 KiB に置く(外へは出さない)。応答の形は `alc_hub_core::alarm_key` と 1 文字も違わない(no_std の写し、`src/proto.rs`)。Secure Boot と OTP は使わない(取り消せない操作は無い)。
- シリアル OTA は §6 と同じ行(`OTA READY 4096 RX=4096` → 4096 B ずつ `OTA ACK <累計>` → `OTA OK`)。書き先は動いていない方の区画(A / B、`partition_table.json`)。イメージは「試してから確定」(TBYB): 再起動後に `OTA CONFIRM` が来れば ROM の `explicit_buy` で確定、10 分来なければウォッチドッグを止めて ROM に前の区画へ戻させ、戻った側が `EVT OTA_ROLLED_BACK slot=<A|B> reason=serial_unconfirmed` を出す。起動直後に落ちるイメージも同じく戻る(ESP の機種の「既知の限界」は無い)。送るのは ELF のフラットなバイナリ(`llvm-objcopy -O binary`)。
- **実機ではまだ動かしていない**(Pico 2 の到着待ち)。区画表の JSON の書き方と ROM の起動情報の読み方は資料どおりで、実機で確かめる。

## 工場での書き込み(1 回だけ)

1. BOOTSEL で起動(Pico 2 はボタンを押しながら USB、t3 は TP1 と TP2 をショートして電源)
2. 区画表: `picotool partition create partition_table.json pt.uf2` → `picotool load pt.uf2` → `picotool reboot -u`(BOOTSEL のまま区画表を効かせる)
3. ファーム: `picotool load -x target/thumbv8m.main-none-eabihf/release/alc-fp001s-rp2354`(A 区画に入る)
4. 起動したら、コンソールに `OTA CONFIRM`(TBYB のイメージは、確定しないと 10 分で止まる)→ `AUTH KEYGEN` → `AUTH PUBKEY` を読んでサーバーに登録

区画表の無い(`picotool load` だけで 0x0 に入れた)開発中の機体は、シリアル OTA を `OTA ERR begin` で断る。

## ビルドと書き込み

ルートの ESP のワークスペースとは別(stable + `thumbv8m.main-none-eabihf`、このディレクトリの `rust-toolchain.toml` / `.cargo/config.toml`)。

```sh
cd crates/fp001s-rp2354
cargo build --release
# BOOTSEL(Pico 2 はボタンを押しながら USB、t3 は TP1 と TP2 をショートして電源)で
picotool load -u -v -x -t elf target/thumbv8m.main-none-eabihf/release/alc-fp001s-rp2354
```
