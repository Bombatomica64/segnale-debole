#!/usr/bin/env bash
# SEGNALE DEBOLE — end-to-end test harness.
#
#   ./scripts/playtest.sh [out_dir]
#
# 1. cargo test: unit tests + Layer 1 headless ECS integration suite
#    (tests/gameplay_integration.rs, incl. the autonomous bot run headless).
# 2. Layer 2: builds the game and runs the live, rendered autonomous playtest
#    bot under xvfb (SEGNALE_BOT_TEST=<out_dir>): storyboard PNGs +
#    playtest_report.json land in <out_dir>.
# 3. Prints the report and exits with the bot's exit code (0 = passed).
#
# Env passthrough: SEGNALE_BOT_SEED (run seed, default 1234),
# SEGNALE_BOT_DT (fixed sim step per frame, default 1/30 s; 0 = real time).
set -uo pipefail

OUT="${1:-$HOME/scratch/segnale-playtest}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT" || exit 2
mkdir -p "$OUT"

echo "== [1/3] cargo test (unit + headless ECS integration) =="
if ! cargo test; then
    echo "playtest: cargo test FAILED" >&2
    exit 2
fi

echo "== [2/3] live rendered playtest bot (xvfb, 1280x720) -> $OUT =="
if ! cargo build; then
    echo "playtest: cargo build FAILED" >&2
    exit 3
fi
command -v xvfb-run >/dev/null || { echo "playtest: xvfb-run not found" >&2; exit 4; }
LOG="$OUT/playtest.log"
start=$(date +%s)
timeout 900 xvfb-run -a -s "-screen 0 1280x720x24" \
    env SEGNALE_BOT_TEST="$OUT" ./target/debug/segnale_debole >"$LOG" 2>&1
BOT=$?
echo "bot exited with code $BOT after $(( $(date +%s) - start ))s (log: $LOG)"
grep -E "playtest bot" "$LOG" | sed 's/\x1b\[[0-9;]*m//g' | sed 's/^.*playtest bot: /  /' || true

echo "== [3/3] $OUT/playtest_report.json =="
if [[ -f "$OUT/playtest_report.json" ]]; then
    cat "$OUT/playtest_report.json"
else
    echo "playtest: no report written" >&2
    tail -n 40 "$LOG" >&2
    [[ $BOT -eq 0 ]] && BOT=5
fi
exit "$BOT"
