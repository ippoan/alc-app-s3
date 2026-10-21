#!/usr/bin/env bash
# 5 つの実行ファイルの main.rs が、自分の crate の版を名乗る登録
# (`set_firmware_version(env!("CARGO_PKG_VERSION"))`) を持っているかを検査する
# (Refs ippoan/alc-app#436)。
#
# **なぜ要るか**: hub-common はライブラリなので、登録が無いと hub-common 自身の版
# (0.1.0) を名乗る。manifest (build.yml) は機種ごとの crate の版を載せるため、
# 食い違うと画面からの更新が not_rebooted で失敗し続ける。コンパイルは通るので
# 実機に焼くまで気づけない。hub-common の単体テストは CI で走らないため grep で見る。
set -euo pipefail

status=0
for f in \
  src/main.rs \
  crates/atoms3-print/src/main.rs \
  crates/atoms3-timecard/src/main.rs \
  crates/atoms3-alarm/src/main.rs \
  crates/atoms3-nfc/src/main.rs; do
  if ! grep -qF 'set_firmware_version(env!("CARGO_PKG_VERSION"))' "$f"; then
    echo "::error file=$f::set_firmware_version(env!(\"CARGO_PKG_VERSION\")) が無い — 機体が hub-common の版を名乗り、manifest と食い違う"
    status=1
  fi
done
exit $status
