/* localist top: per-process and per-socket network byte accounting.
 *
 * Ledgers, one update path per packet:
 *   net_bytes — pid (tgid) → {recv, sent} for TCP+UDP combined (PROCS net Σ)
 *   udp_bytes — pid (tgid) → {recv, sent} UDP only (PROCS UDP breakdown)
 *   udp_sk    — socket INODE → {recv, sent}, UDP only (CONNS)
 *
 * TCP uses the same four syscall symbols for v4 and v6 (tcp_sendmsg /
 * tcp_recvmsg); UDP has separate v6 symbols (udpv6_*).
 *
 * Deliberately header-free: probes read raw register slots of x86_64 struct
 * pt_regs (arg1 = rdi = sk, arg3 = rdx = len for the entry kprobes; retval
 * = rax for the kretprobes, whose arg registers are already clobbered — an
 * entry kprobe stashes sk keyed by the full pid_tgid for its kretprobe).
 * The sk→inode walk is the single struct-coupled part; it uses CO-RE
 * relocations (field names resolved against /sys/kernel/btf/vmlinux at load
 * time), so no per-kernel builds. /proc/net/udp's pointer column is a
 * per-boot HASH on this kernel and cannot be used for correlation, hence
 * the in-kernel walk. Results are cached per sk in an LRU map, so steady
 * state costs one map lookup per packet.
 */

typedef unsigned long long u64;
typedef unsigned int u32;

struct udp_val {
	u64 recv;
	u64 sent;
};

static void *(*map_lookup_elem)(const void *, const void *) = (void *)1;
static long (*map_update_elem)(const void *, const void *, const void *, u64) =
	(void *)2;
static long (*map_delete_elem)(const void *, const void *) = (void *)3;
static u64 (*get_current_pid_tgid)(void) = (void *)14;
static long (*probe_read_kernel)(void *dst, u32 size, const void *src) =
	(void *)113;

/* x86_64 struct pt_regs; syscall args live in rdi/rsi/rdx */
struct pt_regs {
	u64 r15, r14, r13, r12, rbp, rbx, r11, r10, r9, r8;
	u64 rax, rcx, rdx, rsi, rdi, orig_rax, rip, cs, eflags, rsp, ss;
};

/* CO-RE window onto the chain sock→socket→file→inode; local layouts are
 * irrelevant, relocations patch real offsets by field name */
struct sock {
	struct socket *sk_socket;
};
struct socket {
	struct file *file;
};
struct file {
	struct inode *f_inode;
};
struct inode {
	unsigned long i_ino;
};

#define SEC(x) __attribute__((section(x), used))

struct bpf_map_def {
	unsigned int type, key_size, value_size, max_entries, map_flags,
		inner_map_idx, numa_node;
};

/* BPF_MAP_TYPE_LRU_HASH; idle pids/sockets evict themselves, reuse is
 * absorbed by the userspace saturating diff */
SEC("maps") struct bpf_map_def net_bytes = {
	.type = 9,
	.key_size = 4,
	.value_size = 16,
	.max_entries = 16384,
};

SEC("maps") struct bpf_map_def udp_bytes = {
	.type = 9,
	.key_size = 4,
	.value_size = 16,
	.max_entries = 16384,
};

SEC("maps") struct bpf_map_def udp_sk = {
	.type = 9,
	.key_size = 8, /* inode */
	.value_size = 16,
	.max_entries = 16384,
};

/* kretprobe arg registers are clobbered; stash sk between the entry kprobe
 * and its kretprobe, keyed by the full pid_tgid (thread-unique). Shared by
 * the four stashed probes: syscalls of one thread cannot interleave. */
SEC("maps") struct bpf_map_def recv_sk = {
	.type = 9,
	.key_size = 8,
	.value_size = 8,
	.max_entries = 16384,
};

/* sk → inode cache, so the CO-RE walk runs once per socket */
SEC("maps") struct bpf_map_def sk_ino = {
	.type = 9,
	.key_size = 8,
	.value_size = 8,
	.max_entries = 16384,
};

SEC("license") char _license[] = "GPL";

/* A single send()/recv() cannot legitimately report more than this (UDP GSO
 * caps at ~4 MiB; TCP retvals are bounded by socket buffers well below it).
 * On blocking recv paths the kretprobe retval register is occasionally
 * polluted with a timeout in nanoseconds (~1e9) on this kernel, which would
 * otherwise dwarf all real traffic. */
#define MAX_SANE_BYTES (16u << 20)

static __attribute__((always_inline)) int sane_bytes(u64 v)
{
	return v > 0 && v <= MAX_SANE_BYTES;
}

/* a macro, not an inline function taking the map as an argument: the map
 * reference must sit at each call site for clang to emit the map relocation
 * against the right symbol */
#define BUMP(MAP, KEY, BYTES, IS_RECV)                                        \
	do {                                                                  \
		struct udp_val *v_ = map_lookup_elem(MAP, &(KEY));            \
		if (v_) {                                                     \
			if (IS_RECV)                                          \
				__sync_fetch_and_add(&v_->recv, BYTES);       \
			else                                                  \
				__sync_fetch_and_add(&v_->sent, BYTES);       \
			break;                                                \
		}                                                             \
		struct udp_val nv_ = {};                                      \
		if (IS_RECV)                                                  \
			nv_.recv = BYTES;                                     \
		else                                                  \
			nv_.sent = BYTES;                                     \
		/* BPF_NOEXIST: two CPUs racing on a first packet must not    \
		 * overwrite each other; only on a lost race do we fall       \
		 * back to lookup+add */                                      \
		if (map_update_elem(MAP, &(KEY), &nv_, 1) == 0)               \
			break;                                                \
		v_ = map_lookup_elem(MAP, &(KEY));                            \
		if (v_) {                                                     \
			if (IS_RECV)                                          \
				__sync_fetch_and_add(&v_->recv, BYTES);       \
			else                                                  \
				__sync_fetch_and_add(&v_->sent, BYTES);       \
		}                                                             \
	} while (0)

/* sk → inode through socket→file→inode; 0 when anything is missing
 * (kernel sockets have no file) or the walk races a dying socket */
static __attribute__((always_inline)) u64 sk_inode(u64 sk)
{
	void *s = (void *)sk;
	struct socket *sksock = 0;
	struct file *f = 0;
	struct inode *node = 0;
	u64 ino = 0;

	if (probe_read_kernel(&sksock, 8, __builtin_preserve_access_index(
						 &((struct sock *)s)->sk_socket)) ||
	    !sksock)
		return 0;
	if (probe_read_kernel(&f, 8, __builtin_preserve_access_index(
					     &((struct socket *)sksock)->file)) ||
	    !f)
		return 0;
	if (probe_read_kernel(&node, 8, __builtin_preserve_access_index(
						&((struct file *)f)->f_inode)) ||
	    !node)
		return 0;
	if (probe_read_kernel(&ino, 8, __builtin_preserve_access_index(
					       &((struct inode *)node)->i_ino)) ||
	    !ino)
		return 0;
	return ino;
}

/* is_udp: UDP additionally feeds the per-pid breakdown and the per-inode
 * CONNS ledger; TCP rides only the combined pid ledger (CONNS reads its
 * per-socket numbers from the kernel's own tcp_info counters instead) */
static __attribute__((always_inline)) void account(u64 sk, u64 bytes,
						   int is_recv, int is_udp)
{
	u32 pid = get_current_pid_tgid() >> 32;
	BUMP(&net_bytes, pid, bytes, is_recv);

	if (!is_udp)
		return;
	BUMP(&udp_bytes, pid, bytes, is_recv);

	if (!sk)
		return;
	u64 *cached = map_lookup_elem(&sk_ino, &sk);
	u64 ino;
	if (cached) {
		ino = *cached;
	} else {
		ino = sk_inode(sk);
		if (!ino)
			return; /* unaccountable socket, pid side still counts */
		map_update_elem(&sk_ino, &sk, &ino, 1 /* BPF_NOEXIST */);
	}
	BUMP(&udp_sk, ino, bytes, is_recv);
}

static __attribute__((always_inline)) void stash_sk(struct pt_regs *ctx)
{
	u64 id = get_current_pid_tgid();
	u64 sk = ctx->rdi;
	map_update_elem(&recv_sk, &id, &sk, 0);
}

/* ret = syscall retval (bytes on success); is_recv selects the ledger side */
static __attribute__((always_inline)) void take_ret(struct pt_regs *ctx,
						    int is_recv, int is_udp)
{
	u64 id = get_current_pid_tgid();
	long ret = (long)ctx->rax;
	u64 *skp = map_lookup_elem(&recv_sk, &id);
	if (!skp)
		return;
	if (sane_bytes((u64)ret))
		account(*skp, (u64)ret, is_recv, is_udp);
	map_delete_elem(&recv_sk, &id);
}

/* --- UDP (v4 symbols; v6 has its own) --- */

SEC("kprobe/udp_sendmsg")
int udp_send(struct pt_regs *ctx)
{
	u64 len = ctx->rdx;
	if (sane_bytes(len))
		account(ctx->rdi, len, 0, 1);
	return 0;
}

SEC("kprobe/udpv6_sendmsg")
int udp6_send(struct pt_regs *ctx)
{
	u64 len = ctx->rdx;
	if (sane_bytes(len))
		account(ctx->rdi, len, 0, 1);
	return 0;
}

SEC("kprobe/udp_recvmsg")
int udp_recv_entry(struct pt_regs *ctx)
{
	stash_sk(ctx);
	return 0;
}

SEC("kretprobe/udp_recvmsg")
int udp_recv(struct pt_regs *ctx)
{
	take_ret(ctx, 1, 1);
	return 0;
}

SEC("kprobe/udpv6_recvmsg")
int udp6_recv_entry(struct pt_regs *ctx)
{
	stash_sk(ctx);
	return 0;
}

SEC("kretprobe/udpv6_recvmsg")
int udp6_recv(struct pt_regs *ctx)
{
	take_ret(ctx, 1, 1);
	return 0;
}

/* --- TCP (same symbols for v4 and v6; count the retval, not the requested
 * len — a short write on a full send buffer must not overcount) --- */

SEC("kprobe/tcp_sendmsg")
int tcp_send_entry(struct pt_regs *ctx)
{
	stash_sk(ctx);
	return 0;
}

SEC("kretprobe/tcp_sendmsg")
int tcp_send(struct pt_regs *ctx)
{
	take_ret(ctx, 0, 0);
	return 0;
}

SEC("kprobe/tcp_recvmsg")
int tcp_recv_entry(struct pt_regs *ctx)
{
	stash_sk(ctx);
	return 0;
}

SEC("kretprobe/tcp_recvmsg")
int tcp_recv(struct pt_regs *ctx)
{
	take_ret(ctx, 1, 0);
	return 0;
}
