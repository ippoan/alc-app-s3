//! タニタ FC-1200 (ALBLO) RS232 プロトコルの純粋ロジック (ippoan/fc1200-wasm 移植)。
//!
//! 通信は 9600bps 8N1 / CRLF 行指向 (config::RS232_BAUD)。**FC-1200 側が
//! 接続要求 (RQCN) を送ってくる** device-initiated 型で、ホスト (本ハブ) は
//! CNOK / RSOK を返すだけで測定フローが進む:
//!
//! ```text
//! FC → RQCN,FC-1200,B   接続要求        ← CNOK
//! FC → UT,TTTTTT,DDD    センサ使用時間 (ウォームアップ開始)
//! FC → MSWM             ウォームアップ完了
//! FC → MSBL             吹込待ち (繰り返し)
//! FC → MSTO             吹込タイムアウト → 待機へ戻る
//! FC → MSEN             吹込完了 (測定中)
//! FC → RS,RRR,NNNNN     測定結果 (RRR=0.01mg/L 単位) ← RSOK
//! FC → RSOV,NNNNN       レンジオーバー (0.25mg/L 以上) ← RSOK
//! FC → RSERBL,NNNNN     吹込エラー ← RSOK
//! ```
//!
//! fc1200-wasm のメモリ読み出し / 日時設定 / 寿命確認モードは点呼キオスクでは
//! 使わないため移植していない (必要になったら wasm 版 modes.rs を参照)。
//! UART 送受信・イベント fan-out などの副作用は firmware 側 (rs232.rs) が担う。

/// 受信バッファ上限。CRLF が来ないままこれを超えたら壊れた入力とみなし捨てる
const MAX_BUFFER_SIZE: usize = 1024;

/// CRLF 行の組み立てバッファ。UART の分割読み出しを跨いで行を復元する
pub struct LineParser {
    buffer: Vec<u8>,
}

impl Default for LineParser {
    fn default() -> Self {
        Self::new()
    }
}

impl LineParser {
    pub fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(64),
        }
    }

    /// 受信バイト列を追記し、完成した行 (CRLF 除去済み) を返す。
    /// 行未満の端数は内部バッファに残る。
    pub fn feed(&mut self, data: &[u8]) -> Vec<String> {
        self.buffer.extend_from_slice(data);

        if self.buffer.len() > MAX_BUFFER_SIZE {
            self.buffer.clear();
            return vec![];
        }

        let mut lines = Vec::new();
        let mut start = 0;
        while let Some(p) = self.buffer[start..].windows(2).position(|w| w == b"\r\n") {
            let line_end = start + p;
            if let Ok(line) = std::str::from_utf8(&self.buffer[start..line_end]) {
                if !line.is_empty() {
                    lines.push(line.to_string());
                }
            }
            start = line_end + 2;
        }
        if start > 0 {
            self.buffer.drain(..start);
        }
        lines
    }
}

/// FC-1200 → ホストのコマンド
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IncomingCommand {
    /// "RQCN,FC-1200,B" / "RQCNFC-1200B" (実機はカンマ無しも送る)
    ConnectionRequest { model: String, variant: String },
    /// "UT,TTTTTT,DDD" — センサ使用時間 (秒) と経過日数
    UsageTime {
        total_seconds: u32,
        elapsed_days: u16,
    },
    /// "MSWM" — ウォームアップ完了
    WarmingComplete,
    /// "MSBL" — 吹込待ち
    BlowWaiting,
    /// "MSTO" — 吹込タイムアウト
    BlowTimeout,
    /// "MSEN" — 吹込完了
    BlowingFinished,
    /// "RS,RRR,NNNNN" — 正常結果 (RRR = 0.01mg/L 単位の整数)
    NormalResult { alcohol_value: u16, use_count: u32 },
    /// "RSOV,NNNNN" — レンジオーバー (0.25mg/L 以上)
    OverResult { use_count: u32 },
    /// "RSERBL,NNNNN" — 吹込エラー
    BlowError { use_count: u32 },
}

impl IncomingCommand {
    /// 1 行 (CRLF 除去済み) をコマンドに解釈する。不明・不正は None
    pub fn parse(line: &str) -> Option<IncomingCommand> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }

        match trimmed {
            "MSWM" => return Some(IncomingCommand::WarmingComplete),
            "MSBL" => return Some(IncomingCommand::BlowWaiting),
            "MSTO" => return Some(IncomingCommand::BlowTimeout),
            "MSEN" => return Some(IncomingCommand::BlowingFinished),
            _ => {}
        }

        // 実機はカンマ無し形式も送る ("RQCNFC-1200B" 等)。
        // RS 系は長いプレフィックスから先に判定する (RSERBL/RSOV が RS に食われないよう)
        if let Some(rest) = trimmed.strip_prefix("RQCN") {
            if !rest.is_empty() && !rest.starts_with(',') {
                let model = &rest[..rest.len() - 1];
                let variant = &rest[rest.len() - 1..];
                if !model.is_empty() {
                    return Some(IncomingCommand::ConnectionRequest {
                        model: model.to_string(),
                        variant: variant.to_string(),
                    });
                }
            }
        }
        if let Some(rest) = trimmed.strip_prefix("RSERBL") {
            if rest.len() == 5 && rest.bytes().all(|b| b.is_ascii_digit()) {
                return Some(IncomingCommand::BlowError {
                    use_count: rest.parse().ok()?,
                });
            }
        }
        if let Some(rest) = trimmed.strip_prefix("RSOV") {
            if rest.len() == 5 && rest.bytes().all(|b| b.is_ascii_digit()) {
                return Some(IncomingCommand::OverResult {
                    use_count: rest.parse().ok()?,
                });
            }
        }
        if let Some(rest) = trimmed.strip_prefix("RS") {
            if rest.len() == 8 && rest.bytes().all(|b| b.is_ascii_digit()) {
                return Some(IncomingCommand::NormalResult {
                    alcohol_value: rest[..3].parse().ok()?,
                    use_count: rest[3..].parse().ok()?,
                });
            }
        }
        if let Some(rest) = trimmed.strip_prefix("UT") {
            if rest.len() == 9 && rest.bytes().all(|b| b.is_ascii_digit()) {
                return Some(IncomingCommand::UsageTime {
                    total_seconds: rest[..6].parse().ok()?,
                    elapsed_days: rest[6..].parse().ok()?,
                });
            }
        }

        let parts: Vec<&str> = trimmed.split(',').collect();
        match parts[0] {
            "RQCN" if parts.len() >= 3 => Some(IncomingCommand::ConnectionRequest {
                model: parts[1].to_string(),
                variant: parts[2].to_string(),
            }),
            "UT" if parts.len() >= 3 => Some(IncomingCommand::UsageTime {
                total_seconds: parts[1].parse().ok()?,
                elapsed_days: parts[2].parse().ok()?,
            }),
            "RS" if parts.len() >= 3 => Some(IncomingCommand::NormalResult {
                alcohol_value: parts[1].parse().ok()?,
                use_count: parts[2].parse().ok()?,
            }),
            "RSOV" if parts.len() >= 2 => Some(IncomingCommand::OverResult {
                use_count: parts[1].parse().ok()?,
            }),
            "RSERBL" if parts.len() >= 2 => Some(IncomingCommand::BlowError {
                use_count: parts[1].parse().ok()?,
            }),
            _ => None,
        }
    }
}

/// ホスト → FC-1200 の応答コマンド
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutgoingCommand {
    /// "CNOK" — 接続要求の受理
    ConnectionOk,
    /// "RSOK" — 測定結果の受領確認
    ResultOk,
}

impl OutgoingCommand {
    /// CRLF 終端付きの送信バイト列
    pub fn to_bytes(self) -> &'static [u8] {
        match self {
            OutgoingCommand::ConnectionOk => b"CNOK\r\n",
            OutgoingCommand::ResultOk => b"RSOK\r\n",
        }
    }
}

/// 測定結果の種別
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlcoholResult {
    /// 正常測定 (値あり)
    Normal,
    /// レンジオーバー (0.25mg/L 以上)
    Over,
    /// 吹込エラー (値なし)
    BlowError,
}

/// 状態機械が上位 (rs232.rs) へ通知するイベント
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// 接続確立 (CNOK 応答済み)
    Connected { model: String, variant: String },
    /// ウォームアップ開始 (センサ使用時間つき)
    WarmingUp {
        total_seconds: u32,
        elapsed_days: u16,
    },
    /// ウォームアップ完了 → 吹込待ちへ
    BlowWaiting,
    /// 吹込タイムアウト → 待機へ戻った
    BlowTimeout,
    /// 吹込完了 → 測定中
    Measuring,
    /// 測定結果 (RSOK 応答済み)。value は 0.01mg/L 単位の整数
    Result {
        result: AlcoholResult,
        centi_mg_per_l: u16,
        use_count: u32,
    },
    /// 現在の状態では想定しないコマンド (ログ用)
    Unexpected { detail: String },
}

/// 測定フローの状態
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// 待機 (未接続)
    Idle,
    /// 接続済み (UT 待ち)
    Connected,
    /// ウォームアップ中 (MSWM 待ち)
    WarmingUp,
    /// 吹込待ち (MSBL/MSTO/MSEN 待ち)
    BlowWaiting,
    /// 吹込完了・結果待ち (RS/RSOV/RSERBL 待ち)
    Measuring,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::Connected => "connected",
            State::WarmingUp => "warming_up",
            State::BlowWaiting => "blow_waiting",
            State::Measuring => "measuring",
        }
    }
}

/// FC-1200 測定フローの状態機械。キオスク用途のため常時受付
/// (RQCN はどの状態でも接続として受理し、途中状態を破棄して仕切り直す)。
pub struct StateMachine {
    state: State,
    outgoing: Vec<OutgoingCommand>,
}

impl Default for StateMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl StateMachine {
    pub fn new() -> Self {
        Self {
            state: State::Idle,
            outgoing: Vec::new(),
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// 次に送るべき応答コマンドを取り出す (無ければ None)
    pub fn take_response(&mut self) -> Option<OutgoingCommand> {
        if self.outgoing.is_empty() {
            None
        } else {
            Some(self.outgoing.remove(0))
        }
    }

    /// 受信コマンドを処理しイベントを返す
    pub fn process(&mut self, cmd: &IncomingCommand) -> Vec<Event> {
        match (self.state, cmd) {
            // RQCN はどの状態でも接続へ (FC-1200 側の再起動・リトライに追従)
            (_, IncomingCommand::ConnectionRequest { model, variant }) => {
                self.state = State::Connected;
                self.outgoing.clear();
                self.outgoing.push(OutgoingCommand::ConnectionOk);
                vec![Event::Connected {
                    model: model.clone(),
                    variant: variant.clone(),
                }]
            }
            (
                State::Connected,
                IncomingCommand::UsageTime {
                    total_seconds,
                    elapsed_days,
                },
            ) => {
                self.state = State::WarmingUp;
                vec![Event::WarmingUp {
                    total_seconds: *total_seconds,
                    elapsed_days: *elapsed_days,
                }]
            }
            (State::WarmingUp, IncomingCommand::WarmingComplete) => {
                self.state = State::BlowWaiting;
                vec![Event::BlowWaiting]
            }
            // MSBL の繰り返しは状態維持 (イベントも出さない — 毎秒来るため)
            (State::BlowWaiting, IncomingCommand::BlowWaiting) => vec![],
            (State::BlowWaiting, IncomingCommand::BlowTimeout) => {
                self.state = State::Idle;
                vec![Event::BlowTimeout]
            }
            (State::BlowWaiting, IncomingCommand::BlowingFinished) => {
                self.state = State::Measuring;
                vec![Event::Measuring]
            }
            (
                State::Measuring,
                IncomingCommand::NormalResult {
                    alcohol_value,
                    use_count,
                },
            ) => self.finish(AlcoholResult::Normal, *alcohol_value, *use_count),
            (State::Measuring, IncomingCommand::OverResult { use_count }) => {
                // OVER は機器の測定上限 0.25mg/L を値として扱う (wasm 版と同じ)
                self.finish(AlcoholResult::Over, 25, *use_count)
            }
            (State::Measuring, IncomingCommand::BlowError { use_count }) => {
                self.finish(AlcoholResult::BlowError, 0, *use_count)
            }
            (state, cmd) => vec![Event::Unexpected {
                detail: format!("{:?} (state={})", cmd, state.as_str()),
            }],
        }
    }

    fn finish(&mut self, result: AlcoholResult, centi: u16, use_count: u32) -> Vec<Event> {
        self.state = State::Idle;
        self.outgoing.push(OutgoingCommand::ResultOk);
        vec![Event::Result {
            result,
            centi_mg_per_l: centi,
            use_count,
        }]
    }
}

/// 表示値 "0.250" (mg/L、ホストの RESULT 値と同じ 3 桁小数)
pub fn value_str(centi_mg_per_l: u16) -> String {
    format!("{:.3}", f64::from(centi_mg_per_l) / 100.0)
}

/// 点呼の合否: 正常測定かつ 0.000 mg/L のみ OK
pub fn is_pass(result: AlcoholResult, centi_mg_per_l: u16) -> bool {
    result == AlcoholResult::Normal && centi_mg_per_l == 0
}

/// イベントログ行 "アルコール 0.250 mg/L" / "アルコール 測定エラー"
pub fn event_line(result: AlcoholResult, centi_mg_per_l: u16) -> String {
    match result {
        AlcoholResult::Normal => format!("アルコール {} mg/L", value_str(centi_mg_per_l)),
        AlcoholResult::Over => format!("アルコール {} mg/L 以上 (OVER)", value_str(centi_mg_per_l)),
        AlcoholResult::BlowError => "アルコール 測定エラー (吹込不良)".to_string(),
    }
}

/// ホスト JSON / WS uplink (kind="alcohol") 共用の payload。
/// ble-medical-gateway の測定 JSON と同じ 1 行オブジェクト形式
pub fn payload_json(result: AlcoholResult, centi_mg_per_l: u16, use_count: u32) -> String {
    let result_str = match result {
        AlcoholResult::Normal => "normal",
        AlcoholResult::Over => "over",
        AlcoholResult::BlowError => "error",
    };
    format!(
        "{{\"type\":\"alcohol\",\"value\":{},\"unit\":\"mg/L\",\"result\":\"{result_str}\",\"use_count\":{use_count}}}",
        value_str(centi_mg_per_l)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------- LineParser ----------------

    #[test]
    fn parser_complete_and_split_lines() {
        let mut p = LineParser::default();
        assert_eq!(p.feed(b"MSWM\r\n"), vec!["MSWM"]);
        assert!(p.feed(b"MSB").is_empty());
        assert_eq!(p.feed(b"L\r\n"), vec!["MSBL"]);
        // CRLF がチャンク境界で割れるケース
        assert!(p.feed(b"MSEN\r").is_empty());
        assert_eq!(p.feed(b"\n"), vec!["MSEN"]);
        // 複数行 + 端数
        assert_eq!(p.feed(b"MSWM\r\nMSBL\r\nRQ"), vec!["MSWM", "MSBL"]);
        assert_eq!(p.feed(b"CN,FC-1200,B\r\n"), vec!["RQCN,FC-1200,B"]);
    }

    #[test]
    fn parser_skips_empty_and_non_utf8() {
        let mut p = LineParser::new();
        // 空行は返さない
        assert_eq!(p.feed(b"\r\nMSWM\r\n"), vec!["MSWM"]);
        // UTF-8 として不正な行は読み飛ばす
        assert_eq!(p.feed(b"\xFF\xFE\r\nMSBL\r\n"), vec!["MSBL"]);
    }

    #[test]
    fn parser_overflow_clears_buffer() {
        let mut p = LineParser::new();
        assert!(p.feed(&vec![b'A'; MAX_BUFFER_SIZE + 1]).is_empty());
        // クリア後は通常動作に戻る
        assert_eq!(p.feed(b"MSWM\r\n"), vec!["MSWM"]);
    }

    // ---------------- IncomingCommand ----------------

    #[test]
    fn parse_comma_forms() {
        assert_eq!(
            IncomingCommand::parse("RQCN,FC-1200,B"),
            Some(IncomingCommand::ConnectionRequest {
                model: "FC-1200".into(),
                variant: "B".into(),
            })
        );
        assert_eq!(
            IncomingCommand::parse("UT,003600,030"),
            Some(IncomingCommand::UsageTime {
                total_seconds: 3600,
                elapsed_days: 30,
            })
        );
        assert_eq!(
            IncomingCommand::parse("RS,025,00150"),
            Some(IncomingCommand::NormalResult {
                alcohol_value: 25,
                use_count: 150,
            })
        );
        assert_eq!(
            IncomingCommand::parse("RSOV,00200"),
            Some(IncomingCommand::OverResult { use_count: 200 })
        );
        assert_eq!(
            IncomingCommand::parse("RSERBL,00100"),
            Some(IncomingCommand::BlowError { use_count: 100 })
        );
    }

    #[test]
    fn parse_no_comma_forms() {
        // 実機 FC-1200B が送るカンマ無し形式 (実機ログ: RQCNFC-1200B)
        assert_eq!(
            IncomingCommand::parse("RQCNFC-1200B"),
            Some(IncomingCommand::ConnectionRequest {
                model: "FC-1200".into(),
                variant: "B".into(),
            })
        );
        assert_eq!(
            IncomingCommand::parse("UT003600030"),
            Some(IncomingCommand::UsageTime {
                total_seconds: 3600,
                elapsed_days: 30,
            })
        );
        assert_eq!(
            IncomingCommand::parse("RS02500150"),
            Some(IncomingCommand::NormalResult {
                alcohol_value: 25,
                use_count: 150,
            })
        );
        assert_eq!(
            IncomingCommand::parse("RSOV00200"),
            Some(IncomingCommand::OverResult { use_count: 200 })
        );
        assert_eq!(
            IncomingCommand::parse("RSERBL00100"),
            Some(IncomingCommand::BlowError { use_count: 100 })
        );
    }

    #[test]
    fn parse_simple_commands() {
        assert_eq!(
            IncomingCommand::parse("MSWM"),
            Some(IncomingCommand::WarmingComplete)
        );
        assert_eq!(
            IncomingCommand::parse("MSBL"),
            Some(IncomingCommand::BlowWaiting)
        );
        assert_eq!(
            IncomingCommand::parse("MSTO"),
            Some(IncomingCommand::BlowTimeout)
        );
        assert_eq!(
            IncomingCommand::parse("MSEN"),
            Some(IncomingCommand::BlowingFinished)
        );
    }

    #[test]
    fn parse_rejects_invalid() {
        assert_eq!(IncomingCommand::parse(""), None);
        assert_eq!(IncomingCommand::parse("   "), None);
        assert_eq!(IncomingCommand::parse("UNKNOWN"), None);
        assert_eq!(IncomingCommand::parse("RS,abc,def"), None);
        assert_eq!(IncomingCommand::parse("UT,x,y"), None);
        assert_eq!(IncomingCommand::parse("RSOV,x"), None);
        assert_eq!(IncomingCommand::parse("RSERBL,x"), None);
        // "RQCN," はカンマ区切りだが要素不足
        assert_eq!(IncomingCommand::parse("RQCN,"), None);
        // カンマ無し RQCN で variant 1 文字のみ (model 空) は不正
        assert_eq!(IncomingCommand::parse("RQCNX"), None);
        // カンマ無し形式の桁数不一致は不正
        assert_eq!(IncomingCommand::parse("RS123"), None);
        assert_eq!(IncomingCommand::parse("UT12345"), None);
        assert_eq!(IncomingCommand::parse("RSOV123456"), None);
        assert_eq!(IncomingCommand::parse("RSERBL1"), None);
    }

    #[test]
    fn outgoing_bytes() {
        assert_eq!(OutgoingCommand::ConnectionOk.to_bytes(), b"CNOK\r\n");
        assert_eq!(OutgoingCommand::ResultOk.to_bytes(), b"RSOK\r\n");
    }

    // ---------------- StateMachine ----------------

    fn connect(sm: &mut StateMachine) {
        sm.process(&IncomingCommand::ConnectionRequest {
            model: "FC-1200".into(),
            variant: "B".into(),
        });
        assert_eq!(sm.take_response(), Some(OutgoingCommand::ConnectionOk));
    }

    #[test]
    fn full_happy_path() {
        let mut sm = StateMachine::default();
        assert_eq!(sm.state(), State::Idle);
        assert_eq!(sm.take_response(), None);

        let events = sm.process(&IncomingCommand::ConnectionRequest {
            model: "FC-1200".into(),
            variant: "B".into(),
        });
        assert_eq!(
            events,
            vec![Event::Connected {
                model: "FC-1200".into(),
                variant: "B".into(),
            }]
        );
        assert_eq!(sm.state(), State::Connected);
        assert_eq!(sm.take_response(), Some(OutgoingCommand::ConnectionOk));
        assert_eq!(sm.take_response(), None);

        let events = sm.process(&IncomingCommand::UsageTime {
            total_seconds: 3600,
            elapsed_days: 30,
        });
        assert_eq!(
            events,
            vec![Event::WarmingUp {
                total_seconds: 3600,
                elapsed_days: 30,
            }]
        );
        assert_eq!(sm.state(), State::WarmingUp);

        assert_eq!(
            sm.process(&IncomingCommand::WarmingComplete),
            vec![Event::BlowWaiting]
        );
        assert_eq!(sm.state(), State::BlowWaiting);

        // MSBL の繰り返しはイベント無しで状態維持
        assert!(sm.process(&IncomingCommand::BlowWaiting).is_empty());
        assert_eq!(sm.state(), State::BlowWaiting);

        assert_eq!(
            sm.process(&IncomingCommand::BlowingFinished),
            vec![Event::Measuring]
        );
        assert_eq!(sm.state(), State::Measuring);

        let events = sm.process(&IncomingCommand::NormalResult {
            alcohol_value: 0,
            use_count: 150,
        });
        assert_eq!(
            events,
            vec![Event::Result {
                result: AlcoholResult::Normal,
                centi_mg_per_l: 0,
                use_count: 150,
            }]
        );
        assert_eq!(sm.state(), State::Idle);
        assert_eq!(sm.take_response(), Some(OutgoingCommand::ResultOk));
    }

    #[test]
    fn blow_timeout_returns_to_idle() {
        let mut sm = StateMachine::new();
        connect(&mut sm);
        sm.process(&IncomingCommand::UsageTime {
            total_seconds: 100,
            elapsed_days: 1,
        });
        sm.process(&IncomingCommand::WarmingComplete);
        assert_eq!(
            sm.process(&IncomingCommand::BlowTimeout),
            vec![Event::BlowTimeout]
        );
        assert_eq!(sm.state(), State::Idle);
    }

    #[test]
    fn over_result_uses_device_threshold() {
        let mut sm = StateMachine::new();
        connect(&mut sm);
        sm.process(&IncomingCommand::UsageTime {
            total_seconds: 100,
            elapsed_days: 1,
        });
        sm.process(&IncomingCommand::WarmingComplete);
        sm.process(&IncomingCommand::BlowingFinished);
        let events = sm.process(&IncomingCommand::OverResult { use_count: 200 });
        assert_eq!(
            events,
            vec![Event::Result {
                result: AlcoholResult::Over,
                centi_mg_per_l: 25,
                use_count: 200,
            }]
        );
        assert_eq!(sm.take_response(), Some(OutgoingCommand::ResultOk));
    }

    #[test]
    fn blow_error_result() {
        let mut sm = StateMachine::new();
        connect(&mut sm);
        sm.process(&IncomingCommand::UsageTime {
            total_seconds: 100,
            elapsed_days: 1,
        });
        sm.process(&IncomingCommand::WarmingComplete);
        sm.process(&IncomingCommand::BlowingFinished);
        let events = sm.process(&IncomingCommand::BlowError { use_count: 100 });
        assert_eq!(
            events,
            vec![Event::Result {
                result: AlcoholResult::BlowError,
                centi_mg_per_l: 0,
                use_count: 100,
            }]
        );
        assert_eq!(sm.take_response(), Some(OutgoingCommand::ResultOk));
    }

    #[test]
    fn rqcn_mid_flow_resets_and_reconnects() {
        let mut sm = StateMachine::new();
        connect(&mut sm);
        sm.process(&IncomingCommand::UsageTime {
            total_seconds: 100,
            elapsed_days: 1,
        });
        assert_eq!(sm.state(), State::WarmingUp);
        // 途中で FC-1200 が再起動 → RQCN で仕切り直し (未送信の応答は破棄)
        let events = sm.process(&IncomingCommand::ConnectionRequest {
            model: "FC-1200".into(),
            variant: "B".into(),
        });
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], Event::Connected { .. }));
        assert_eq!(sm.state(), State::Connected);
        assert_eq!(sm.take_response(), Some(OutgoingCommand::ConnectionOk));
        assert_eq!(sm.take_response(), None);
    }

    #[test]
    fn unexpected_command_emits_event() {
        let mut sm = StateMachine::new();
        // Idle で結果が来た (取りこぼした接続の残骸など)
        let events = sm.process(&IncomingCommand::NormalResult {
            alcohol_value: 25,
            use_count: 1,
        });
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], Event::Unexpected { detail } if detail.contains("state=idle")),
            "{events:?}"
        );
        assert_eq!(sm.state(), State::Idle);
    }

    #[test]
    fn state_as_str_all_variants() {
        assert_eq!(State::Idle.as_str(), "idle");
        assert_eq!(State::Connected.as_str(), "connected");
        assert_eq!(State::WarmingUp.as_str(), "warming_up");
        assert_eq!(State::BlowWaiting.as_str(), "blow_waiting");
        assert_eq!(State::Measuring.as_str(), "measuring");
    }

    // ---------------- 整形ヘルパ ----------------

    #[test]
    fn value_and_pass_judgement() {
        assert_eq!(value_str(0), "0.000");
        assert_eq!(value_str(25), "0.250");
        assert_eq!(value_str(7), "0.070");
        assert!(is_pass(AlcoholResult::Normal, 0));
        assert!(!is_pass(AlcoholResult::Normal, 7));
        assert!(!is_pass(AlcoholResult::Over, 25));
        assert!(!is_pass(AlcoholResult::BlowError, 0));
    }

    #[test]
    fn event_lines() {
        assert_eq!(event_line(AlcoholResult::Normal, 0), "アルコール 0.000 mg/L");
        assert_eq!(
            event_line(AlcoholResult::Over, 25),
            "アルコール 0.250 mg/L 以上 (OVER)"
        );
        assert_eq!(
            event_line(AlcoholResult::BlowError, 0),
            "アルコール 測定エラー (吹込不良)"
        );
    }

    #[test]
    fn payload_json_shapes() {
        assert_eq!(
            payload_json(AlcoholResult::Normal, 0, 150),
            r#"{"type":"alcohol","value":0.000,"unit":"mg/L","result":"normal","use_count":150}"#
        );
        assert_eq!(
            payload_json(AlcoholResult::Over, 25, 200),
            r#"{"type":"alcohol","value":0.250,"unit":"mg/L","result":"over","use_count":200}"#
        );
        assert_eq!(
            payload_json(AlcoholResult::BlowError, 0, 100),
            r#"{"type":"alcohol","value":0.000,"unit":"mg/L","result":"error","use_count":100}"#
        );
    }
}
