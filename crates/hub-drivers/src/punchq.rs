//! オフライン送信キューの保存先 (専用 NVS パーティション `punchq`、Refs #142)。
//!
//! `hub-core::uplink::UplinkStore` の firmware 実装を 2 つ持つ:
//!
//! - [`NvsKeyStore`] — 専用パーティション `punchq` に **1 件 1 キー** (キーは
//!   seq の 16 進)。push = 1 キー書き、ack = 1 キー消し。512 KB で打刻
//!   (1 件 175 B) 約 2,280 件。
//! - [`LegacyStringStore`] — `punchq` を持たない機 (OTA だけで更新した機。
//!   **パーティションテーブルは OTA では書かれない**) 向けの退避先。既定 nvs の
//!   `ws_queue` 文字列 1 キーに改行区切りで入れる従来方式で、上限は 20 件、
//!   1 件書くたびに全件を書き戻す旧コストのまま。
//!
//! ## 電断時の意味
//!
//! 旧方式は「全件が書き戻るか、全件が旧状態のまま」だったが、1 件 1 キーでは
//! **書き途中の 1 件だけが欠け得る** (NVS のキー書きは原子的)。溜まった過去の
//! 打刻を巻き添えにしないぶん安全側。
//!
//! ## ホストへのイベント出力
//!
//! | イベント | 意味 |
//! |---|---|
//! | `EVT PUNCHQ nvs count=<n>` | 専用パーティションで起動 (n = 未送信件数) |
//! | `EVT PUNCHQ legacy count=<n>` | パーティションが無く既定 nvs へフォールバック |
//! | `EVT PUNCHQ migrated <n>` | 旧 `ws_queue` の n 件を punchq へ移した |

use alc_hub_common::settings::Settings;
use alc_hub_core::uplink::{parse_line, seq_key, StoreFull, UplinkStore, MAX_LINE_BYTES};
use esp_idf_svc::nvs::{EspCustomNvsPartition, EspNvs, NvsCustom};
use esp_idf_svc::sys::ESP_ERR_NVS_NOT_ENOUGH_SPACE;

/// パーティションテーブル (partitions.csv) 上の名前
const PARTITION: &str = "punchq";
/// 名前空間。パーティション専用なので名前は同じでよい
const NAMESPACE: &str = "punchq";
/// get_str のバッファ。1 行は MAX_LINE_BYTES 未満 (+ NUL)。
/// ws_uplink スレッドのスタックは 20KB しかないのでヒープに置く
const READ_BUF: usize = MAX_LINE_BYTES + 96;
/// フォールバック時の保持件数 (既定 nvs の文字列 4KB に収まる上限)
const LEGACY_MAX: usize = 20;

/// 専用 NVS パーティションに 1 件 1 キーで置く実装
pub struct NvsKeyStore {
    nvs: EspNvs<NvsCustom>,
}

impl UplinkStore for NvsKeyStore {
    fn put(&mut self, seq: u64, line: &str) -> Result<(), StoreFull> {
        match self.nvs.set_str(&seq_key(seq), line) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == ESP_ERR_NVS_NOT_ENOUGH_SPACE => Err(StoreFull),
            Err(e) => {
                // 容量以外の失敗 (I/O・上限超過) も「入らなかった」として扱う。
                // 呼び側は最古を捨てて 1 回だけ再試行する
                log::error!("punchq: 保存失敗 seq={seq}: {e:?}");
                Err(StoreFull)
            }
        }
    }

    fn remove(&mut self, seq: u64) {
        if let Err(e) = self.nvs.remove(&seq_key(seq)) {
            log::warn!("punchq: 削除失敗 seq={seq}: {e:?}");
        }
    }

    fn get(&self, seq: u64) -> Option<String> {
        let mut buf = vec![0u8; READ_BUF];
        match self.nvs.get_str(&seq_key(seq), &mut buf) {
            Ok(v) => v.map(str::to_string),
            Err(e) => {
                log::warn!("punchq: 読み出し失敗 seq={seq}: {e:?}");
                None
            }
        }
    }

    fn seqs(&self) -> Vec<u64> {
        let mut out = Vec::new();
        // **unsafe の nvs_entry_find は書かない** — esp-idf-svc の keys() を使う
        match self.nvs.keys(None) {
            Ok(mut keys) => {
                while let Some((key, _)) = keys.next_key() {
                    match u64::from_str_radix(key, 16) {
                        Ok(seq) => out.push(seq),
                        Err(_) => log::warn!("punchq: 想定外のキー {key} を無視"),
                    }
                }
            }
            Err(e) => log::error!("punchq: キー列挙失敗: {e:?}"),
        }
        out
    }
}

/// `punchq` を持たない機の退避先 (既定 nvs の `ws_queue` 文字列、上限 20 件)。
/// **旧方式のまま** — 1 件の put で全件を書き戻す
pub struct LegacyStringStore {
    settings: Settings,
}

impl LegacyStringStore {
    /// 保存済みの (seq, 行) を seq 昇順で読む。壊れた行は落とす
    fn rows(&self) -> Vec<(u64, String)> {
        let text = self.settings.ws_queue();
        let mut rows: Vec<(u64, String)> = text
            .lines()
            .filter_map(|l| parse_line(l).map(|e| (e.seq, l.to_string())))
            .collect();
        rows.sort_by_key(|(seq, _)| *seq);
        rows
    }

    fn save(&self, rows: &[(u64, String)]) {
        let text: Vec<&str> = rows.iter().map(|(_, l)| l.as_str()).collect();
        self.settings.set_ws_queue(&text.join("\n"));
    }
}

impl UplinkStore for LegacyStringStore {
    fn put(&mut self, seq: u64, line: &str) -> Result<(), StoreFull> {
        let mut rows = self.rows();
        match rows.iter().position(|(s, _)| *s == seq) {
            Some(i) => rows[i].1 = line.to_string(),
            None => {
                if rows.len() >= LEGACY_MAX {
                    return Err(StoreFull);
                }
                rows.push((seq, line.to_string()));
                rows.sort_by_key(|(s, _)| *s);
            }
        }
        // 既定 nvs の文字列 1 キーに収まらなければ容量不足として扱う
        let total: usize = rows.iter().map(|(_, l)| l.len() + 1).sum();
        if total >= MAX_LINE_BYTES {
            return Err(StoreFull);
        }
        self.save(&rows);
        Ok(())
    }

    fn remove(&mut self, seq: u64) {
        let mut rows = self.rows();
        rows.retain(|(s, _)| *s != seq);
        self.save(&rows);
    }

    fn get(&self, seq: u64) -> Option<String> {
        self.rows()
            .into_iter()
            .find(|(s, _)| *s == seq)
            .map(|(_, l)| l)
    }

    fn seqs(&self) -> Vec<u64> {
        self.rows().into_iter().map(|(seq, _)| seq).collect()
    }
}

/// 保存先を開く。`punchq` があればそちら (旧 `ws_queue` に残っていれば移行して
/// から)、無ければ既定 nvs の文字列へフォールバックする。
/// 戻り値の `&'static str` は STATUS / ログ用のモード名
pub fn open_store(settings: &Settings) -> (Box<dyn UplinkStore>, &'static str) {
    match EspCustomNvsPartition::take(PARTITION)
        .and_then(|part| EspNvs::new(part, NAMESPACE, true))
    {
        Ok(nvs) => {
            let mut store = NvsKeyStore { nvs };
            migrate_legacy(settings, &mut store);
            let count = store.seqs().len();
            log::info!("punchq: 専用パーティションを使用 (未送信 {count} 件)");
            println!("EVT PUNCHQ nvs count={count}");
            (Box::new(store), "nvs")
        }
        Err(e) => {
            // パーティションテーブルは OTA では更新されないので、OTA だけで
            // 上げた機はここへ来る (異常ではない)
            log::warn!("punchq: 専用パーティションを開けません ({e:?}) — 既定 nvs へフォールバック");
            let store = LegacyStringStore {
                settings: settings.clone(),
            };
            let count = store.seqs().len();
            println!("EVT PUNCHQ legacy count={count}");
            (Box::new(store), "legacy")
        }
    }
}

/// 旧形式 (既定 nvs の `ws_queue` 文字列) に残っている未送信ぶんを punchq へ移す。
/// **seq は保ったまま**移す (再利用すると ack が別のエントリを消し込む)
fn migrate_legacy(settings: &Settings, store: &mut NvsKeyStore) {
    let text = settings.ws_queue();
    if text.trim().is_empty() {
        return;
    }
    let mut moved = 0usize;
    for line in text.lines() {
        let Some(entry) = parse_line(line) else {
            continue;
        };
        match store.put(entry.seq, line) {
            Ok(()) => moved += 1,
            Err(_) => log::error!("punchq: 移行できませんでした seq={}", entry.seq),
        }
    }
    settings.set_ws_queue("");
    log::info!("punchq: 旧キューから {moved} 件を移行");
    println!("EVT PUNCHQ migrated {moved}");
}
