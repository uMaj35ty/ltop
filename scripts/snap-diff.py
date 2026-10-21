#!/usr/bin/env python3
"""snap-diff — ltop-bpf 快照差分诊断工具。

排查"PROCS 和总流量对不上"类问题的第一手证据:连续采样
/run/lanmon/udp.json,输出每个 pid 的原始计数增量、计数回退(会被
ltop 钳 0 的事件)、以及 pid 增减。只读快照,无需 root。

用法:
  scripts/snap-diff.py                     # 采 3 轮,间隔 1s
  scripts/snap-diff.py -n 5 -g 2 -t 10     # 5 轮、间隔 2s、显示前 10 名
  scripts/snap-diff.py -p /tmp/other.json  # 检查别的快照文件
"""
import argparse
import json
import sys
import time


def load(path):
    with open(path) as f:
        s = json.load(f)
    by_pid = lambda arr: {e["pid"]: (e["recv"], e["sent"]) for e in arr}
    by_ino = lambda arr: {e["ino"]: (e["recv"], e["sent"]) for e in arr}
    return s["ts"], by_pid(s["pids"]), by_pid(s.get("upids", [])), by_ino(s.get("sks", []))


def main():
    ap = argparse.ArgumentParser(description="ltop-bpf 快照差分诊断")
    ap.add_argument("-n", "--times", type=int, default=3, help="采样轮数(默认 3)")
    ap.add_argument("-g", "--gap", type=float, default=1.0, help="轮间隔秒(默认 1)")
    ap.add_argument("-t", "--top", type=int, default=8, help="每轮显示前 N 名(默认 8)")
    ap.add_argument("-p", "--path", default="/run/lanmon/udp.json", help="快照路径")
    args = ap.parse_args()

    prev = None
    for rnd in range(args.times):
        try:
            ts, pids, upids, sks = load(args.path)
        except (OSError, json.JSONDecodeError) as e:
            sys.exit(f"读 {args.path} 失败: {e}")
        age = time.time() * 1000 - ts
        print(f"── 第 {rnd + 1} 轮  ts 距今 {age:.0f}ms  pids {len(pids)}  "
              f"upids {len(upids)}  sockets {len(sks)}")
        if age > 10_000:
            print("   ⚠ 快照超过 10 秒未更新:daemon 没在跑?")
        if prev is not None:
            dt = (ts - prev[0]) / 1000
            rows = []
            for pid in set(prev[1]) | set(pids):
                if pid not in prev[1]:
                    rows.append((pid, "新现", 0, 0))
                    continue
                if pid not in pids:
                    rows.append((pid, "消失", 0, 0))
                    continue
                dr = pids[pid][0] - prev[1][pid][0]
                ds = pids[pid][1] - prev[1][pid][1]
                if dr < 0 or ds < 0:
                    rows.append((pid, f"回退! Δ({dr:+}, {ds:+})", dr, ds))
                elif dr or ds:
                    rows.append((pid, f"Δrecv {dr:+} Δsent {ds:+}", dr, ds))
            rows.sort(key=lambda r: -(r[2] + r[3] if isinstance(r[2], int) else 0))
            for pid, what, *_ in rows[: args.top]:
                print(f"   pid {pid:<8} {what}")
            if not rows:
                print("   (无增量)")
            lost = [p for p in prev[1] if p not in pids]
            if lost:
                print(f"   消失 pid: {lost} (LRU 逐出/进程退出,增量已按基线钳 0)")
        print()
        prev = (ts, pids, upids, sks)
        if rnd + 1 < args.times:
            time.sleep(args.gap)


if __name__ == "__main__":
    main()
