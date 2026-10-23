//! Compiles bpf/udp_bpf.c with the system clang into an eBPF object that is
//! embedded into the binaries. When clang or the BPF target is unavailable,
//! an empty object is embedded instead: the loader side then degrades
//! silently (the TUI) or refuses to start with a clear message (ltop-bpf).

use std::path::PathBuf;
use std::process::Command;

fn main() {
    // eBPF 仅 Linux;mac 上嵌空对象(TUI 侧 UDP 分项静默缺席,其余不受影响)
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
        std::fs::write(
            out.join("bpf_object.rs"),
            "pub static BPF_OBJECT: &[u8] = &[];\n",
        )
        .unwrap();
        return;
    }

    let src = PathBuf::from("bpf/net_bpf.c");
    println!("cargo:rerun-if-changed={}", src.display());
    println!("cargo:rerun-if-env-changed=CLANG");

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let inc = out.join("bpf_object.rs");
    let obj = out.join("ltop_udp_bpf.o");
    let clang = std::env::var("CLANG").unwrap_or_else(|_| "clang".into());

    // 探针 C 按 BPF 目标编译(字节码本身无架构),但 pt_regs 的字段布局
    // 取自主机架构——net_bpf.c 按 libbpf 惯例依赖该宏做 #if 分支
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let arch_define = match target_arch.as_str() {
        "x86_64" => "-D__TARGET_ARCH_x86",
        "aarch64" => "-D__TARGET_ARCH_aarch64",
        other => {
            println!("cargo:warning=不支持的架构 {other}:嵌入空 eBPF 对象,UDP 计费将不可用");
            std::fs::write(&inc, "pub static BPF_OBJECT: &[u8] = &[];\n").unwrap();
            return;
        }
    };

    let ok = Command::new(&clang)
        .args([
            "-O2",
            "-g",
            "-target",
            "bpf",
            "-Wall",
            "-Wextra",
            "-Wno-unused-parameter",
            arch_define,
            "-c",
        ])
        .arg(&src)
        .arg("-o")
        .arg(&obj)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if ok {
        std::fs::write(
            &inc,
            format!(
                "pub static BPF_OBJECT: &[u8] = include_bytes!({:?});\n",
                obj
            ),
        )
        .unwrap();
    } else {
        println!(
            "cargo:warning=clang 无法生成 eBPF 对象({clang});UDP 每进程计费将不可用,其余功能不受影响"
        );
        std::fs::write(&inc, "pub static BPF_OBJECT: &[u8] = &[];\n").unwrap();
    }
}
