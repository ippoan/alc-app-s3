# ESP-IDF component manifest for M5Utility.
# Upstream: https://github.com/m5stack/M5Utility (originally Arduino library;
# pure C++ utilities + freertos/esp_timer based compatibility shims, no
# Arduino headers required when ARDUINO is not defined).
#
# Dependencies are PRIV_REQUIRES (implementation-only, not exposed via public
# headers) — all used solely in src/m5_utility/compatibility_feature.cpp.
# Only esp_timer (esp_timer_get_time) needs to be listed; freertos
# (vTaskDelay/xPortInIsrContext) and esp_rom (esp_rom_delay_us) are common
# requirements auto-added to every component, so they are intentionally omitted.
file(GLOB_RECURSE SRCS "src/m5_utility/*.cpp")
idf_component_register(
    SRCS ${SRCS}
    INCLUDE_DIRS "src"
    PRIV_REQUIRES esp_timer
)

# M5_LOG_LEVEL 未定義だと library_log.hpp の logOutputLevel が None になり、
# M5_LIB_LOGE 含む M5 系ライブラリの全ログ (nfcbTransmit の "Illegal mode"、
# wait_for_FIFO の "Error: IRQ" 等) がコンパイル時に完全に消えて失敗が沈黙する
# (issue #96 の調査を長期化させた根本要因、2026-07-21)。PUBLIC 指定で本ヘッダを
# include する M5Unit-NFC / M5UnitUnified / nfc_shim にも波及させる。
# 1=Error のみ。診断時は 4=Debug に上げると REQB 毎の Timeout まで見える
target_compile_definitions(${COMPONENT_LIB} PUBLIC M5_LOG_LEVEL=1)
