#!/bin/bash
# coverage_100.toml に登録されたファイルが 100% ラインカバレッジを維持しているか検証する
# (ippoan/rust-alc-api scripts/check_coverage_100.sh の簡易版 — 本リポジトリの
# カバレッジ対象は crates/hub-core の unit テストのみで、DB や mock 統合テストが
# 無いため type 別の実行モードは持たない)
#
# Usage:
#   bash scripts/check_coverage_100.sh <llvm-cov-summary-file>
#     <llvm-cov-summary-file>: `cargo llvm-cov -p alc-hub-core --summary-only` の出力
set -euo pipefail

SUMMARY="${1:?usage: check_coverage_100.sh <llvm-cov-summary-file>}"
CONFIG="coverage_100.toml"
[[ -f "$CONFIG" ]] || { echo "ERROR: $CONFIG not found"; exit 1; }
[[ -f "$SUMMARY" ]] || { echo "ERROR: $SUMMARY not found"; exit 1; }

mapfile -t PATHS < <(sed -n 's/^path = "\(.*\)"/\1/p' "$CONFIG")
if [[ ${#PATHS[@]} -eq 0 ]]; then
  echo "ERROR: $CONFIG に登録ファイルがありません (パース失敗の可能性)"
  exit 1
fi

echo "=== Coverage 100% Check ==="
echo "Registered files: ${#PATHS[@]}"
echo ""

FAIL=0
for p in "${PATHS[@]}"; do
  base=$(basename "$p")
  # llvm-cov summary の Filename 列は共通ディレクトリが省略されるため、
  # フルパス一致 or ベース名一致で探す (登録ファイルのベース名は一意である前提。
  # 列構成: Filename Regions Missed Cover Functions Missed Executed
  #         Lines MissedLines Cover [Branches Missed Cover])
  row=$(awk -v full="$p" -v base="$base" '$1 == full || $1 == base { print; exit }' "$SUMMARY")
  if [[ -z "$row" ]]; then
    echo "NG  $p: summary に見つからない (テストでリンクされていない?)"
    FAIL=1
    continue
  fi
  missed=$(echo "$row" | awk '{print $(NF-4)}')
  cover=$(echo "$row" | awk '{print $(NF-3)}')
  if [[ "$missed" == "0" ]]; then
    echo "OK  $p (lines 100%)"
  else
    echo "NG  $p: missed lines=$missed (line cover=$cover)"
    FAIL=1
  fi
done

if [[ "$FAIL" -ne 0 ]]; then
  echo ""
  echo "カバレッジ 100% が割れています。テストを追加するか、対象外にする明確な理由が"
  echo "あれば coverage_100.toml から外して PR で説明してください。"
fi
exit $FAIL
