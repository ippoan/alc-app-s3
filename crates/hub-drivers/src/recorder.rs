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

use alc_hub_common::{
    measurement::Measurement,
    settings::Settings,
    status::{event_timestamp, SharedStatus},
    ui_api::UiCommand,
};
use alc_hub_core::vitals;
use anyhow::Result;

pub fn start(
    meas_rx: Receiver<Measurement>,
    ui_tx: Sender<UiCommand>,
    status: SharedStatus,
    settings: Settings,
) -> Result<()> {
    std::thread::Builder::new()
        .name("recorder".into())
        .stack_size(8 * 1024)
        .spawn(move || {
            for m in meas_rx {
                match m {
                    Measurement::Temperature { celsius, at_ms } => {
                        println!(
                            "{{\"type\":\"temperature\",\"value\":{celsius:.1},\"unit\":\"celsius\"}}"
                        );
                        record(&status, &settings, at_ms, &vitals::temp_event(celsius));
                        let _ = ui_tx.send(UiCommand::Temperature { celsius });
                    }
                    Measurement::BloodPressure {
                        systolic,
                        diastolic,
                        pulse,
                        at_ms,
                    } => {
                        match pulse {
                            Some(p) if p > 0.0 => println!(
                                "{{\"type\":\"blood_pressure\",\"systolic\":{systolic:.0},\"diastolic\":{diastolic:.0},\"pulse\":{p:.0},\"unit\":\"mmHg\"}}"
                            ),
                            _ => println!(
                                "{{\"type\":\"blood_pressure\",\"systolic\":{systolic:.0},\"diastolic\":{diastolic:.0},\"unit\":\"mmHg\"}}"
                            ),
                        }
                        record(&status, &settings, at_ms, &vitals::bp_event(systolic, diastolic, pulse));
                        let _ = ui_tx.send(UiCommand::BloodPressure {
                            systolic,
                            diastolic,
                            pulse,
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
