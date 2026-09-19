#!/usr/bin/env bash
# Atom VoiceS3R build (ATOMS3_NFC_VOICES3R) の sdkconfig が本当に overlay で
# 上書きされたかを検査する (Refs ippoan/alc-app#353)。
#
# **なぜ要るか**: 上書きは環境変数 ESP_IDF_SDKCONFIG_DEFAULTS (`;` 区切り) で
# かけている。これが外れる・綴りが変わる・step から env が落ちると、
# esp-idf-sys は Cargo.toml の metadata (= AtomS3 Lite 用の sdkconfig.defaults)
# に**静かに戻り**、PSRAM も NimBLE も無いイメージが「通った」ことになる。
# PSRAM は CONFIG_SPIRAM_IGNORE_NOTFOUND で無くても boot するので、
# **実機に焼いて血圧が動かないまで誰も気づかない**。コンパイルでは分からない。
#
# check job (PR) と build job (push:main) の両方から呼ぶ。
set -euo pipefail

# 復元したキャッシュに前回の成果物が残ることがあるので、**一番新しいもの**を見る
SDKCONFIG=$(find target -path '*esp-idf-sys*' -name sdkconfig -printf '%T@ %p\n' 2>/dev/null \
  | sort -nr | head -1 | cut -d' ' -f2-)

if [ -z "$SDKCONFIG" ]; then
  echo "::error::生成された sdkconfig が見つかりません (esp-idf-sys のビルドが走っていない?)"
  exit 1
fi
echo "sdkconfig: $SDKCONFIG"

# OCT: QUAD だと PSRAM が初期化できず、IGNORE_NOTFOUND で黙って PSRAM 無しで起動する
# EXTERNAL: NimBLE ホストのヒープを PSRAM へ寄せる (無いと BLE_INIT: Malloc failed)
for key in CONFIG_SPIRAM_MODE_OCT=y CONFIG_BT_NIMBLE_MEM_ALLOC_MODE_EXTERNAL=y; do
  if ! grep -qx -- "$key" "$SDKCONFIG"; then
    echo "::error::$key が sdkconfig にありません — ESP_IDF_SDKCONFIG_DEFAULTS の上書きが効いていません"
    exit 1
  fi
  echo "ok: $key"
done
