//! esp-idf-sys の cfg フラグ (esp_idf_eth_spi_ethernet_w5500 等) を本クレートの
//! コンパイルに伝搬する (esp-idf-svc/esp-idf-hal 自身の build.rs と同じ定石)。
//! これが無いと #[cfg(esp_idf_...)] が常に偽になり、sdkconfig で有効化した
//! 機能のモジュールがコンパイルされない。
fn main() {
    embuild::espidf::sysenv::relay();
    embuild::espidf::sysenv::output();
    // embuild 0.33 は rustc-check-cfg を出さないため、sdkconfig 依存 cfg を
    // 未知条件として警告されないよう自前で宣言する
    println!("cargo::rustc-check-cfg=cfg(esp_idf_eth_spi_ethernet_w5500)");
}
