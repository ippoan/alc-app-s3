//! 測定値レコーダ: BLE の notify コールバックから受けた測定値を、
//! ホストへの JSON 出力 / イベントログ / NVS 永続化 / 画面通知 に振り分ける。
//!
//! notify コールバックは nimble_host タスク上 (スタック小) で走るため、
//! 重い処理をそこでやると血圧受信時にスタックオーバーフローして再起動していた。
//! このレコーダを専用スレッド (十分なスタック) で回し、コールバックは
//! 「パースして Measurement を送るだけ」に留める。
//!
//! 測定値は NVS にも追記され、リブートしても「ログ確認」画面に残る。

use std::sync::mpsc::{Receiver, Sender};
use std::time::{SystemTime, UNIX_EPOCH};

use alc_hub_common::{
    measurement::{Measurement, UplinkRecord},
    settings::Settings,
    status::{event_timestamp, SharedStatus},
    ui_api::UiCommand,
};
use alc_hub_core::{fc1200, vitals};
use anyhow::Result;

/// 現在の epoch ms (NTP 未同期時は 1970 起点の稼働時間になる — サーバ側は
/// recorded_at_ms をそのまま保存するだけなので許容し、受信時刻で補完する)
fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn start(
    meas_rx: Receiver<Measurement>,
    ui_tx: Sender<UiCommand>,
    status: SharedStatus,
    settings: Settings,
    ws_tx: Sender<UplinkRecord>,
) -> Result<()> {
    std::thread::Builder::new()
        .name("recorder".into())
        .stack_size(8 * 1024)
        .spawn(move || {
            // 直近に記録した測定の (機器タイムスタンプ, 測定値)。同一測定の
            // 再送 (送信済み機器への再接続時など) を二重記録しないための比較値。
            // NT-100B のタイムスタンプは分単位 (秒は常に 00 — 実機で確認) の
            // ため、時刻だけで判定すると同じ分内の測り直しまで捨ててしまう。
            // 値も含めて完全一致した場合のみ再送とみなす
            let mut last_temp: Option<(u64, f32)> = None;
            let mut last_bp: Option<(u64, f32, f32, Option<f32>)> = None;
            // FC-1200 は累計使用回数 (use_count) が測定ごとに増えるため、
            // RSOK 取りこぼしによる再送は use_count 一致で見分ける
            let mut last_alc: Option<u32> = None;
            for m in meas_rx {
                match m {
                    Measurement::Temperature {
                        celsius,
                        timestamp,
                        at_ms,
                    } => {
                        if let Some(ts) = timestamp {
                            if last_temp == Some((ts, celsius)) {
                                log::info!("recorder: 体温の再送を無視 ({ts} {celsius:.1})");
                                continue;
                            }
                            last_temp = Some((ts, celsius));
                        }
                        let json = match timestamp {
                            Some(ts) => format!(
                                "{{\"type\":\"temperature\",\"value\":{celsius:.1},\"unit\":\"celsius\",\"measured_at\":{ts}}}"
                            ),
                            None => format!(
                                "{{\"type\":\"temperature\",\"value\":{celsius:.1},\"unit\":\"celsius\"}}"
                            ),
                        };
                        println!("{json}");
                        // WS 送信 (cf-alc-recorder) へも同じ payload を fan-out
                        let _ = ws_tx.send(UplinkRecord {
                            kind: "temperature",
                            payload: json,
                            recorded_at_ms: epoch_ms(),
                        });
                        record(&status, &settings, at_ms, &vitals::temp_event(celsius));
                        let _ = ui_tx.send(UiCommand::Temperature { celsius });
                    }
                    Measurement::BloodPressure {
                        systolic,
                        diastolic,
                        pulse,
                        timestamp,
                        at_ms,
                    } => {
                        if let Some(ts) = timestamp {
                            if last_bp == Some((ts, systolic, diastolic, pulse)) {
                                log::info!(
                                    "recorder: 血圧の再送を無視 ({ts} {systolic:.0}/{diastolic:.0})"
                                );
                                continue;
                            }
                            last_bp = Some((ts, systolic, diastolic, pulse));
                        }
                        let pulse_part = match pulse {
                            Some(p) if p > 0.0 => format!(",\"pulse\":{p:.0}"),
                            _ => String::new(),
                        };
                        let ts_part = match timestamp {
                            Some(ts) => format!(",\"measured_at\":{ts}"),
                            None => String::new(),
                        };
                        let json = format!(
                            "{{\"type\":\"blood_pressure\",\"systolic\":{systolic:.0},\"diastolic\":{diastolic:.0}{pulse_part},\"unit\":\"mmHg\"{ts_part}}}"
                        );
                        println!("{json}");
                        let _ = ws_tx.send(UplinkRecord {
                            kind: "blood_pressure",
                            payload: json,
                            recorded_at_ms: epoch_ms(),
                        });
                        record(&status, &settings, at_ms, &vitals::bp_event(systolic, diastolic, pulse));
                        let _ = ui_tx.send(UiCommand::BloodPressure {
                            systolic,
                            diastolic,
                            pulse,
                        });
                    }
                    Measurement::Alcohol {
                        result,
                        centi_mg_per_l,
                        use_count,
                        at_ms,
                    } => {
                        if last_alc == Some(use_count) {
                            log::info!("recorder: アルコールの再送を無視 (use_count={use_count})");
                            continue;
                        }
                        last_alc = Some(use_count);
                        let json = fc1200::payload_json(result, centi_mg_per_l, use_count);
                        println!("{json}");
                        let _ = ws_tx.send(UplinkRecord {
                            kind: "alcohol",
                            payload: json,
                            recorded_at_ms: epoch_ms(),
                        });
                        record(
                            &status,
                            &settings,
                            at_ms,
                            &fc1200::event_line(result, centi_mg_per_l),
                        );
                        let _ = ui_tx.send(UiCommand::Result {
                            ok: fc1200::is_pass(result, centi_mg_per_l),
                            value: fc1200::value_str(centi_mg_per_l),
                        });
                    }
                }
            }
        })?;
    Ok(())
}

/// RAM のイベントログ (画面表示用) と NVS の測定ログ (永続) の両方に追記する。
/// 両者で同じ行 (時刻 + 内容) を使い、リブート後も同じ表示になるようにする。
/// 時刻ラベルは接続イベント等と共通 (event_timestamp): NTP 同期済みなら
/// 日本時間 MM/DD HH:MM:SS、未同期なら稼働時間。
fn record(status: &SharedStatus, settings: &Settings, at_ms: u64, event: &str) {
    let line = format!("{} {event}", event_timestamp(at_ms));
    if let Ok(mut st) = status.lock() {
        st.push_line(line.clone());
    }
    settings.append_measurement_log(&line);
}
