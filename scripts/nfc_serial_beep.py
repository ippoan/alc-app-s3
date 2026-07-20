#!/usr/bin/env python3
"""COM ポートのシリアル出力を監視し、行を受信するたびに PC 側でビープ音を鳴らす。

alc-app-s3 の NFC 検証で、実機の画面を見なくても「反応があったかどうか」を
音で分かるようにするためのデバッグ用ツール。ファームウェア側は log::info!/
println! で何か1行出すだけでよい (内容による分岐は --match で絞り込み可能)。

起動ログ (boot:/esp_image:/heap_init: 等) は毎回大量に出るため、既定では
NFC 関連のキーワードにマッチした行だけで鳴動する (--match "" で全行に戻せる)。

使い方:
    python scripts/nfc_serial_beep.py                  # COM10, 115200bps, NFC関連行のみ鳴動
    python scripts/nfc_serial_beep.py --port COM10
    python scripts/nfc_serial_beep.py --match "IDm|免許"   # マッチパターンを変える
    python scripts/nfc_serial_beep.py --match ""        # 空文字指定で全行鳴動に戻す
"""
import argparse
import re
import sys
import time

import serial
import winsound

DEFAULT_PORT = "COM10"
DEFAULT_BAUD = 115200
DEFAULT_MATCH = "NFC|免許|IDm"
BEEP_FREQ_HZ = 1500
BEEP_MS = 150


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--port", default=DEFAULT_PORT, help=f"COM port (default: {DEFAULT_PORT})")
    ap.add_argument("--baud", type=int, default=DEFAULT_BAUD, help=f"baud rate (default: {DEFAULT_BAUD})")
    ap.add_argument(
        "--match",
        default=DEFAULT_MATCH,
        help=f'この正規表現に一致した行だけ鳴動 (既定: "{DEFAULT_MATCH}"、空文字で全行鳴動)',
    )
    args = ap.parse_args()

    pattern = re.compile(args.match) if args.match else None

    print(f"[nfc_serial_beep] watching {args.port} @ {args.baud}bps (Ctrl+C to stop)")
    if pattern:
        print(f"[nfc_serial_beep] match pattern: {args.match}")

    while True:
        try:
            with serial.Serial(args.port, args.baud, timeout=1) as ser:
                print(f"[nfc_serial_beep] connected to {args.port}")
                while True:
                    raw = ser.readline()
                    if not raw:
                        continue
                    line = raw.decode("utf-8", errors="replace").rstrip("\r\n")
                    if not line:
                        continue
                    ts = time.strftime("%H:%M:%S")
                    if pattern is None or pattern.search(line):
                        print(f"[{ts}] BEEP <- {line}")
                        winsound.Beep(BEEP_FREQ_HZ, BEEP_MS)
                    else:
                        print(f"[{ts}]        {line}")
        except serial.SerialException as e:
            print(f"[nfc_serial_beep] serial error: {e} -- retrying in 3s")
            time.sleep(3)
        except KeyboardInterrupt:
            print("\n[nfc_serial_beep] stopped")
            return 0


if __name__ == "__main__":
    sys.exit(main())
