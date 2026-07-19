# vendor 一覧 (Unit NFC 検証、issue #84 / plan/nfc-card-identity.md)

各ライブラリは ESP Component Registry 未登録 (git 相互依存で解決する形態) のため、
`develop` ブランチのコミットをピン留めしてソースをコピーしている
(git submodule ではない — CI のネットワーク往復排除・内容ベースキャッシュ方針に合わせるため)。
`docs/` `examples/` `test/` `boards/` 等、各リポジトリ自身の `idf_component.yml` の
`files.exclude` に列挙されているビルド非対象ディレクトリは削除済み。

| ディレクトリ | 取得元 | コミット | 日付 |
|---|---|---|---|
| `M5Unit-NFC` | https://github.com/m5stack/M5Unit-NFC | `93745b547364f310cd64b5155a870103a7800a5d` | 2026-06-10 |
| `M5UnitUnified` | https://github.com/m5stack/M5UnitUnified | `bf711f370047cf16355b00005450ef615fab36e2` | 2026-06-09 |
| `M5HAL` | https://github.com/m5stack/M5HAL | `0f06f9d3134706ce030fd5515601cce65a267233` | 2026-06-08 |
| `M5Utility` | https://github.com/m5stack/M5Utility | `301a6b5c6413875e1dd80b027e0639921972b433` | 2026-07-14 |

依存関係: `M5Unit-NFC` → `M5UnitUnified` + `M5Utility` → `M5HAL` → `M5Utility`。
いずれも ESP-IDF native ビルド対応 (M5GFX/M5Unified/Arduino への必須依存なし — 各
`CMakeLists.txt` の `idf_component_register` 参照)。`M5Unified` は
`idf_component_optional_requires` で任意連携するのみ。

更新する場合: 該当ディレクトリを削除し、新しいコミットで同じ手順を繰り返し、
このファイルのコミットハッシュを更新すること。
