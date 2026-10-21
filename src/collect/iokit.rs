//! IOKit IOMedia statistics — the macOS stand-in for /proc/diskstats.
//!
//! Every IOMedia object in the IOService plane carries a "Statistics"
//! dictionary with cumulative "Bytes (Read)" / "Bytes (Write)" /
//! "Operations (Read)" / "Operations (Write)" counters — the same numbers
//! `iostat` shows. Whole-disk medias (disk0) aggregate their slices, so only
//! LEAF medias (no children) are counted, which also skips the synthesized
//! APFS container disk (disk3) whose volumes would double-count disk0s1.
//!
//! Raw externs against the IOKit/CoreFoundation frameworks — no bindings
//! crate. Offsets/kinds: kCFNumberSInt64Type = 4, kCFStringEncodingUTF8 =
//! 0x08000100, default mach port = 0, kCFAllocatorDefault = NULL.

#![allow(dead_code)]

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};

/// IOKit 句柄都是 mach_port_t(u32),非指针;CF 对象才是指针
type IoObject = u32;

/// BSD device name -> (bytes_read, bytes_written, ops_read, ops_write)
/// for leaf IOMedias; plus the full name list for the device tree.
pub fn disk_counters() -> (HashMap<String, [u64; 4]>, Vec<String>) {
    let mut out = HashMap::new();
    let mut names = Vec::new();
    unsafe {
        // SAFETY: standard IOKit iteration; every object and CF value is
        // released on all paths, matched buffers are null-checked.
        let matching = IOServiceMatching(c"IOMedia".as_ptr());
        if matching.is_null() {
            return (out, names);
        }
        let mut it: IoObject = 0;
        if IOServiceGetMatchingServices(0, matching, &mut it) != 0 {
            return (out, names); // consumed `matching` either way
        }
        loop {
            let obj: IoObject = IOIteratorNext(it);
            if obj == 0 {
                break;
            }
            if let Some(name) = string_prop(obj, "BSD Name\0") {
                if bool_prop(obj, "Leaf\0") {
                    if let Some(stats) = dict_prop(obj, "Statistics\0") {
                        let br = number_in_dict(stats, "Bytes (Read)\0");
                        let bw = number_in_dict(stats, "Bytes (Write)\0");
                        let or_ = number_in_dict(stats, "Operations (Read)\0");
                        let ow = number_in_dict(stats, "Operations (Write)\0");
                        CFRelease(stats);
                        out.insert(name.clone(), [br, bw, or_, ow]);
                    }
                }
                names.push(name);
            }
            IOObjectRelease(obj);
        }
        IOObjectRelease(it);
    }
    (out, names)
}

/// whole-disk medias (no `s<N>` suffix) — tree roots
pub fn whole_disks(names: &[String]) -> Vec<String> {
    let mut roots: Vec<String> = names
        .iter()
        .filter(|n| !is_slice(n))
        .cloned()
        .collect();
    roots.sort();
    roots.dedup();
    roots
}

/// "disk3s1s1" -> Some("disk3"); whole-disk names -> None
pub fn slice_parent(name: &str) -> Option<String> {
    if !is_slice(name) {
        return None;
    }
    // 去掉末尾的 s<N>(可重复:disk5s1s2 → disk5)
    let mut cur = name.to_string();
    while let Some(idx) = cur.rfind('s') {
        if cur[idx + 1..].bytes().all(|b| b.is_ascii_digit()) && !cur[idx + 1..].is_empty() {
            cur.truncate(idx);
        } else {
            break;
        }
    }
    if cur.is_empty() || cur == name {
        None
    } else {
        Some(cur)
    }
}

fn is_slice(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("disk") else {
        return false;
    };
    let digits = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        return false;
    }
    let tail = &rest[digits..];
    tail.contains('s')
        && tail.split('s').skip(1).all(|p| {
            !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())
        })
}

unsafe fn string_prop(obj: IoObject, key: &str) -> Option<String> {
    let cf = prop(obj, key)?;
    let mut buf = [0 as c_char; 256];
    let ok = CFStringGetCString(cf, buf.as_mut_ptr(), buf.len(), 0x0800_0100);
    CFRelease(cf);
    if ok {
        Some(cstr_to_string(&buf))
    } else {
        None
    }
}

unsafe fn bool_prop(obj: IoObject, key: &str) -> bool {
    let Some(cf) = prop(obj, key) else {
        return false;
    };
    let v = CFBooleanGetValue(cf);
    CFRelease(cf);
    v
}

unsafe fn dict_prop(obj: IoObject, key: &str) -> Option<*mut c_void> {
    let cf = prop(obj, key)?;
    if CFGetTypeID(cf) == CFDictionaryGetTypeID() {
        Some(cf)
    } else {
        CFRelease(cf);
        None
    }
}

unsafe fn number_in_dict(dict: *mut c_void, key: &str) -> u64 {
    let cfkey = CFStringCreateWithCString(
        std::ptr::null_mut(),
        key.as_ptr() as *const c_char,
        0x0800_0100,
    );
    let v = CFDictionaryGetValue(dict, cfkey);
    CFRelease(cfkey);
    if v.is_null() {
        return 0;
    }
    let mut out: u64 = 0;
    if CFNumberGetValue(v, 4, &mut out as *mut u64 as *mut c_void) {
        out
    } else {
        0
    }
}

unsafe fn prop(obj: IoObject, key: &str) -> Option<*mut c_void> {
    let cfkey = CFStringCreateWithCString(
        std::ptr::null_mut(),
        key.as_ptr() as *const c_char,
        0x0800_0100,
    );
    let cf = IORegistryEntryCreateCFProperty(obj, cfkey, std::ptr::null_mut(), 0);
    CFRelease(cfkey);
    if cf.is_null() {
        None
    } else {
        Some(cf)
    }
}

unsafe fn cstr_to_string(buf: &[c_char]) -> String {
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|&&c| c != 0)
        // 经 c_int 中转:aarch64 的 c_char 是 u8,直转 u8 是同类型 cast
        .map(|&c| c as libc::c_int as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingServices(
        port: IoObject,
        matching: *mut c_void,
        existing: *mut IoObject,
    ) -> c_int;
    fn IOIteratorNext(iterator: IoObject) -> IoObject;
    fn IOObjectRelease(object: IoObject) -> c_int;
    fn IORegistryEntryCreateCFProperty(
        entry: IoObject,
        key: *mut c_void,
        allocator: *mut c_void,
        options: u32,
    ) -> *mut c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithCString(
        alloc: *mut c_void,
        c_str: *const c_char,
        encoding: u32,
    ) -> *mut c_void;
    fn CFStringGetCString(s: *mut c_void, buffer: *mut c_char, buffer_size: usize, encoding: u32)
        -> bool;
    fn CFBooleanGetValue(boolean: *mut c_void) -> bool;
    fn CFDictionaryGetValue(dict: *mut c_void, key: *const c_void) -> *const c_void;
    fn CFNumberGetValue(number: *const c_void, the_type: usize, value_ptr: *mut c_void) -> bool;
    fn CFGetTypeID(cf: *mut c_void) -> usize;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFRelease(cf: *mut c_void);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slice_parent_grouping() {
        assert_eq!(slice_parent("disk0").as_deref(), None);
        assert_eq!(slice_parent("disk0s1").as_deref(), Some("disk0"));
        assert_eq!(slice_parent("disk3s1s1").as_deref(), Some("disk3"));
        assert_eq!(slice_parent("disk3s123").as_deref(), Some("disk3"));
        // 不是分区形态的名字不归组
        assert_eq!(slice_parent("disk5").as_deref(), None);
        assert_eq!(slice_parent("en0").as_deref(), None);
    }

    #[test]
    fn slice_shape_detection() {
        assert!(is_slice("disk0s1"));
        assert!(is_slice("disk3s1s1"));
        assert!(!is_slice("disk0"));
        assert!(!is_slice("disk10"));
        assert!(!is_slice("disk0s"));
        assert!(!is_slice("disks1"));
    }
}
