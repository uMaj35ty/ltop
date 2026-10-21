#!/usr/bin/env bash
# UDP 每进程计费冒烟验证:在 ltop-bpf 采样的同时发一组可对账的双向 UDP 测试流,
# 然后打印快照,核对发送/接收字节数。
#
# 用法:  sudo bash scripts/udp-smoke.sh
# 原理:  --once 挂上探针立即读一次就退出,计数永远是空的;必须让探针保持挂载
#         的同时有流量穿越 udp_sendmsg/udp_recvmsg。本脚本起一个 300ms 间隔的
#         后台 ltop-bpf,发 10×(120B 回环 + 120B 网关 discard),停 1.5s 读快照。
set -u
cd "$(dirname "$0")/.."
BIN=./target/release/ltop-bpf
[[ -x $BIN ]] || BIN=/usr/local/bin/ltop-bpf
[[ -x $BIN ]] || { echo "找不到 ltop-bpf 二进制(先 cargo build --release)"; exit 1; }

GW=$(ip route show default 2>/dev/null | awk '{print $3; exit}')
echo "网关: ${GW:-未找到,跳过网关包}"

if pkill -x ltop-bpf 2>/dev/null; then
    for _ in $(seq 20); do
        pgrep -x ltop-bpf >/dev/null || break
        sleep 0.2
    done
fi
if pgrep -x ltop-bpf >/dev/null; then
    echo "ltop-bpf 已在运行,直接复用现有实例(flock 保证全局单实例)"
else
    "$BIN" --interval 300 &
    DAEMON=$!
    trap 'kill $DAEMON 2>/dev/null' EXIT
    sleep 1 # 等 kprobe attach 完成
fi

GW="$GW" python3 - <<'PY'
import os, socket, threading, time
payload = b"LANMON-UDP-SMOKE" + b"x" * 106          # 每包 120 B
srv = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
srv.bind(("127.0.0.1", 0))
cli = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
srv_addr = srv.getsockname()

def listen():
    for _ in range(10):
        srv.recvfrom(2048)

t = threading.Thread(target=listen, daemon=True)
t.start()
for _ in range(10):
    cli.sendto(payload, srv_addr)                    # 回环:发+收都会被记账
    if os.environ.get("GW"):
        cli.sendto(payload, (os.environ["GW"], 9))   # 网关 discard:只有发送侧
    time.sleep(0.05)
t.join(timeout=3)
print(f"测试进程 pid={os.getpid()}  期望 sent≈{10*240} B  recv≈{10*120} B(均记在本 pid)")
PY

sleep 1.5
echo "--- 快照 /run/ltop/udp.json:"
python3 -m json.tool /run/ltop/udp.json 2>/dev/null || cat /run/ltop/udp.json
echo
echo "解读: pids 里找上面的 pid —— recv/sent 累计值应包含上述数字;"
echo "其他 pid(systemd-resolved 的 DNS 等)出现属正常,内核全局探针看到的都是真实流量。"
