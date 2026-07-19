# NFC 検証用シム component (Unit NFC / ST25R3916)。
# M5UnitUnified (C++) を extern "C" で薄く包み、Rust (crates/hub-drivers/src/nfc.rs) から
# リンクさせる。C++ 例外/RTTI は M5Unit-NFC/M5UnitUnified が std::string/std::vector を
# 使うため既定で必要 (sdkconfig.defaults 側で有効化する。cmake 側の追加設定は不要)。
idf_component_register(
    SRCS "nfc_shim.cpp"
    INCLUDE_DIRS "include"
    REQUIRES M5Unit-NFC M5UnitUnified
)
