//! OTA イメージ URL をビルドの版に合わせて読み替える。
//!
//! CoreS3 には LAN 版 (既定、`alc-hub-cores3-app.bin`) と Wi-Fi 版
//! (`lan` feature 無し、`alc-hub-cores3-wifi-app.bin`) がある。遠隔 OTA の
//! 送り手 (/device/setup) は版を区別せず LAN 版 (または dev 版) の URL を送るため、
//! Wi-Fi 版の機がそのまま書くと Wi-Fi ごと失い二度と繋がらない。
//! 受け手の Wi-Fi 版が自分の版の URL へ読み替える。

const LAN_IMAGES: [&str; 2] = ["alc-hub-cores3-app.bin", "alc-hub-cores3-dev-app.bin"];
const WIFI_IMAGE: &str = "alc-hub-cores3-wifi-app.bin";

/// Wi-Fi 版の機が書くべき URL。ファイル名が CoreS3 の LAN 版 / dev 版なら
/// Wi-Fi 版に差し替え (query / fragment は保つ)、それ以外はそのまま返す。
pub fn wifi_image_url(url: &str) -> String {
    let tail_at = url.find(['?', '#']).unwrap_or(url.len());
    let (path, tail) = url.split_at(tail_at);
    let name_at = path.rfind('/').map_or(0, |i| i + 1);
    if LAN_IMAGES.contains(&&path[name_at..]) {
        format!("{}{WIFI_IMAGE}{tail}", &path[..name_at])
    } else {
        url.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "https://ippoan.github.io/alc-app-s3/firmware/";

    #[test]
    fn lan_image_becomes_wifi() {
        assert_eq!(
            wifi_image_url(&format!("{BASE}alc-hub-cores3-app.bin")),
            format!("{BASE}alc-hub-cores3-wifi-app.bin")
        );
    }

    #[test]
    fn dev_image_becomes_wifi() {
        assert_eq!(
            wifi_image_url(&format!("{BASE}alc-hub-cores3-dev-app.bin")),
            format!("{BASE}alc-hub-cores3-wifi-app.bin")
        );
    }

    #[test]
    fn query_and_fragment_are_kept() {
        assert_eq!(
            wifi_image_url(&format!("{BASE}alc-hub-cores3-app.bin?v=abc#x")),
            format!("{BASE}alc-hub-cores3-wifi-app.bin?v=abc#x")
        );
    }

    #[test]
    fn bare_file_name() {
        assert_eq!(wifi_image_url("alc-hub-cores3-app.bin"), WIFI_IMAGE);
    }

    #[test]
    fn other_urls_unchanged() {
        for url in [
            format!("{BASE}alc-hub-cores3-wifi-app.bin"),
            format!("{BASE}alc-hub-atoms3-print-app.bin"),
            format!("{BASE}x-alc-hub-cores3-app.bin"),
            "https://example.com/?f=alc-hub-cores3-app.bin".to_string(),
        ] {
            assert_eq!(wifi_image_url(&url), url);
        }
    }
}
