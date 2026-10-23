#!/bin/bash
# 资源 soak:TUI + watch + ltop-bpf 每 30s 采样一次(CPU%/RSS),跑 DURATION 秒。
# 用法:scripts/soak.sh [秒数,默认 3600] [输出目录,默认 /tmp/ltop-soak-<ts>]
# 产物:<out>/samples.csv(CSV:ts,proc,cpu,rss[,lines])、watch.jsonl
DURATION=${1:-3600}
OUT=${2:-/tmp/ltop-soak-$(date +%H%M%S)}
mkdir -p "$OUT"
L=${L:-$HOME/.cargo/bin/ltop}
command -v tmux >/dev/null || { echo "需要 tmux 承载 TUI"; exit 1; }
tmux kill-session -t soak-tui 2>/dev/null
tmux new-session -d -s soak-tui -x 160 -y 40 "$L"
"$L" watch --interval 5 --duration "${DURATION}s" --out "$OUT/watch.jsonl" >/dev/null 2>&1 &
sleep 3
for i in $(seq 1 $((DURATION / 30))); do
  ts=$(date +%s)
  tpid=$(pgrep -f "cargo/bin/ltop$" | head -1)
  [ -n "$tpid" ] && { read cpu rss <<< $(ps -o %cpu=,rss= -p "$tpid" --no-headers); echo "$ts,tui,$cpu,$rss" >> "$OUT/samples.csv"; }
  wpid=$(pgrep -f "watch --interval 5" | head -1)
  if [ -n "$wpid" ]; then
    read cpu rss <<< $(ps -o %cpu=,rss= -p "$wpid" --no-headers)
    echo "$ts,watch,$cpu,$rss,lines=$(wc -l < "$OUT/watch.jsonl" 2>/dev/null || echo 0)" >> "$OUT/samples.csv"
  fi
  bpid=$(pgrep -x ltop-bpf | head -1)
  [ -n "$bpid" ] && { read cpu rss <<< $(ps -o %cpu=,rss= -p "$bpid" --no-headers); echo "$ts,bpf,$cpu,$rss" >> "$OUT/samples.csv"; }
  sleep 30
done
tmux kill-session -t soak-tui 2>/dev/null
echo "soak 完成:$OUT/samples.csv"
