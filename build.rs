//! Compiles bpf/udp_bpf.c with the system clang into an eBPF object that is
//! embedded into the binaries. When clang or the BPF target is unavailable,
//! an empty object is embedded instead: the loader side then degrades
//! silently (the TUI) or refuses to start with a clear message (ltop-bpf).

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let src = PathBuf::from("bpf/net_bpf.c");
    println!("cargo:rerun-if-changed={}", src.display());
    println!("cargo:rerun-if-env-changed=CLANG");

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let inc = out.join("bpf_object.rs");
    let obj = out.join("lanmon_udp_bpf.o");
    let clang = std::env::var("CLANG").unwrap_or_else(|_| "clang".into());

    let ok = Command::new(&clang)
        .args([
            "-O2",
            "-g",
            "-target",
            "bpf",
            "-Wall",
            "-Wextra",
            "-Wno-unused-parameter",
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
            format!("pub static BPF_OBJECT: &[u8] = include_bytes!({:?});\n", obj),
        )
        .unwrap();
    } else {
        println!(
            "cargo:warning=clang 无法生成 eBPF 对象({clang});UDP 每进程计费将不可用,其余功能不受影响"
        );
        std::fs::write(&inc, "pub static BPF_OBJECT: &[u8] = &[];\n").unwrap();
    }
}
