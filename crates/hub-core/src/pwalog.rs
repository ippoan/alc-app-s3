//! キオスク PWA の診断ログの中継 — 純粋部分 (Refs ippoan/alc-app-s3#215)。
//!
//! WS 下り `get_log` を受けた CoreS3 は、既に出している
//! `EVT WS_COMMAND <id> <payload>` を合図に、USB で繋がった運行者 PWA が返す
//! `PWALOG <id> <行>` × 最大 40 行 → `PWALOG END <id> <n>` を最大 [`WAIT_MS`]
//! 待って集め、`get_log` の応答の `pwa_log` に入れる。ここは行の解釈・集め方・
//! 切り詰めを持つ。受信スレッドからの受け渡し (channel) と待ち時間の計測は
//! firmware 側 (hub-drivers/src/pwalog.rs)。
//!
//! `<id>` は get_log の command id (= 世代番号)。時間切れの後に遅れて届いた行が
//! 次の get_log に混ざらないよう、今の id と違う行は捨てる。

use std::collections::VecDeque;

use serde_json::Value;

use crate::crashlog::{sanitize_log, tail_lines};

/// PWA の返事の行頭。
pub const PREFIX: &str = "PWALOG ";
/// `pwa_log` の上限 (バイト、末尾を行境界で残す)。
pub const MAX_BYTES: usize = 1200;
/// `END` を待つ上限 (ms)。
pub const WAIT_MS: u64 = 2000;

/// `PWALOG` の 1 行。
#[derive(Debug, PartialEq, Eq)]
pub enum Line<'a> {
    /// `PWALOG <id> <行>` (`<行>` は空でもよい)
    Text { id: &'a str, text: &'a str },
    /// `PWALOG END <id> <n>`
    End { id: &'a str },
}

/// 行を解釈する。`PWALOG ` で始まらなければ `None`。
pub fn parse(line: &str) -> Option<Line<'_>> {
    let rest = line.strip_prefix(PREFIX)?;
    if let Some(end) = rest.strip_prefix("END ") {
        let id = end.split_once(' ').map_or(end, |(id, _)| id);
        return Some(Line::End { id });
    }
    let (id, text) = rest.split_once(' ').unwrap_or((rest, ""));
    Some(Line::Text { id, text })
}

/// 集めた結果。
#[derive(Debug, PartialEq, Eq)]
pub enum PwaLog {
    /// USB ホスト (PWA) が居ない — 待たずに返した
    NoHost,
    /// 受け取った行 (sanitize 済み・末尾 [`MAX_BYTES`])。`complete` = `END` まで届いた
    Received { text: String, complete: bool },
}

impl PwaLog {
    /// `get_log` 応答の `(pwa_log, pwa_log_error)`。
    ///
    /// - 居ない → `(null, "no_host")`
    /// - `END` まで届いた → `(行, null)`
    /// - 時間切れ → `(届いた分 (0 行なら空文字), "timeout")`
    pub fn json_fields(&self) -> (Value, Value) {
        match self {
            Self::NoHost => (Value::Null, "no_host".into()),
            Self::Received { text, complete } => (
                text.as_str().into(),
                if *complete {
                    Value::Null
                } else {
                    "timeout".into()
                },
            ),
        }
    }
}

/// 1 回の get_log ぶんの `PWALOG` 行を集める。
pub struct Collector {
    id: String,
    lines: VecDeque<String>,
    bytes: usize,
    done: bool,
}

impl Collector {
    pub fn new(id: &str) -> Self {
        Self {
            id: id.to_string(),
            lines: VecDeque::new(),
            bytes: 0,
            done: false,
        }
    }

    /// 1 行を受け取る。戻り値 `true` = `END` を受けた (これ以上待たなくてよい)。
    ///
    /// id の違う行・`PWALOG` でない行・`END` の後の行は捨てる。溜める量は
    /// [`MAX_BYTES`] 程度に抑える (古い行から捨てる) — 行数の約束を守らない
    /// 相手でもメモリを食わない
    pub fn feed(&mut self, line: &str) -> bool {
        match parse(line) {
            Some(Line::Text { id, text }) if id == self.id && !self.done => {
                self.bytes += text.len() + 1;
                self.lines.push_back(text.to_string());
                while self.bytes > MAX_BYTES && self.lines.len() > 1 {
                    let old = self.lines.pop_front().expect("len > 1");
                    self.bytes -= old.len() + 1;
                }
            }
            Some(Line::End { id }) if id == self.id => self.done = true,
            _ => {}
        }
        self.done
    }

    /// 集めた行を `\n` で連結し、制御文字を除いて末尾 [`MAX_BYTES`] (行境界) に切る。
    pub fn finish(self) -> PwaLog {
        let joined = Vec::from(self.lines).join("\n");
        let clean = sanitize_log(joined.as_bytes());
        let (tail, _) = tail_lines(&clean, MAX_BYTES);
        PwaLog::Received {
            text: tail.to_string(),
            complete: self.done,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_text_and_end_lines() {
        assert_eq!(
            parse("PWALOG c1 12:00:00.000 open ok"),
            Some(Line::Text {
                id: "c1",
                text: "12:00:00.000 open ok"
            })
        );
        // 行が空 (末尾の空白は受信側の trim で落ちる)
        assert_eq!(parse("PWALOG c1"), Some(Line::Text { id: "c1", text: "" }));
        assert_eq!(parse("PWALOG END c1 3"), Some(Line::End { id: "c1" }));
        assert_eq!(parse("PWALOG END c1"), Some(Line::End { id: "c1" }));
    }

    #[test]
    fn parse_rejects_other_lines() {
        assert_eq!(parse("PING"), None);
        assert_eq!(parse("PWALOG"), None);
        assert_eq!(parse("HB OK"), None);
    }

    #[test]
    fn collector_keeps_only_matching_id_until_end() {
        let mut c = Collector::new("c2");
        assert!(!c.feed("PWALOG c1 stale")); // 前の get_log の遅れた行
        assert!(!c.feed("PWALOG c2 a"));
        assert!(!c.feed("PING"));
        assert!(!c.feed("PWALOG END c1 1")); // 前の END では終わらない
        assert!(!c.feed("PWALOG c2 b"));
        assert!(c.feed("PWALOG END c2 2"));
        assert!(c.feed("PWALOG c2 after")); // END の後は捨てる
        assert_eq!(
            c.finish(),
            PwaLog::Received {
                text: "a\nb".into(),
                complete: true
            }
        );
    }

    #[test]
    fn collector_timeout_keeps_what_arrived() {
        let mut c = Collector::new("c1");
        c.feed("PWALOG c1 a");
        assert_eq!(
            c.finish(),
            PwaLog::Received {
                text: "a".into(),
                complete: false
            }
        );
        // 1 行も来なかった (古い PWA) → 空文字
        assert_eq!(
            Collector::new("c1").finish(),
            PwaLog::Received {
                text: String::new(),
                complete: false
            }
        );
    }

    #[test]
    fn collector_strips_control_chars() {
        let mut c = Collector::new("c1");
        c.feed("PWALOG c1 a\x1b[0;31mb\tc");
        c.feed("PWALOG END c1 1");
        assert_eq!(
            c.finish(),
            PwaLog::Received {
                text: "abc".into(),
                complete: true
            }
        );
    }

    #[test]
    fn collector_truncates_to_newest_max_bytes() {
        let mut c = Collector::new("c1");
        // 100 バイト × 30 行 = 3 KB を送られても末尾 1200 バイト以内に収める
        for i in 0..30 {
            c.feed(&format!("PWALOG c1 {i:02}{}", "x".repeat(98)));
        }
        c.feed("PWALOG END c1 30");
        let (text, error) = c.finish().json_fields();
        assert!(error.is_null(), "END まで届いたはず");
        let text = text.as_str().unwrap();
        assert!(text.len() <= MAX_BYTES, "{}", text.len());
        // 新しい行が残り、行の途中から始まらない
        assert!(text.ends_with(&format!("29{}", "x".repeat(98))));
        assert!(text.lines().all(|l| l.len() == 100));
    }

    #[test]
    fn json_fields_per_outcome() {
        assert_eq!(PwaLog::NoHost.json_fields(), (Value::Null, "no_host".into()));
        let done = PwaLog::Received {
            text: "a".into(),
            complete: true,
        };
        assert_eq!(done.json_fields(), ("a".into(), Value::Null));
        let partial = PwaLog::Received {
            text: String::new(),
            complete: false,
        };
        assert_eq!(partial.json_fields(), ("".into(), "timeout".into()));
    }
}
