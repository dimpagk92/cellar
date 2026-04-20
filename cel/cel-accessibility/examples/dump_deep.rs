//! Deep dump — shows ALL attributes for each element to understand Electron apps.
//!
//! Usage: cargo run -p cel-accessibility --example dump_deep

use core_foundation::array::CFArray;
use core_foundation::base::{CFType, TCFType};
use core_foundation::string::{CFString, CFStringRef};
use std::ffi::c_void;
use std::ptr;

type AXUIElementRef = *const c_void;
type AXError = i32;
use core_foundation::base::CFTypeRef;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        el: AXUIElementRef,
        attr: CFStringRef,
        value: *mut CFTypeRef,
    ) -> AXError;
    fn AXUIElementCopyAttributeNames(
        el: AXUIElementRef,
        names: *mut CFTypeRef,
    ) -> AXError;
    fn CFRelease(cf: *const c_void);
}

fn main() {
    // Get frontmost PID
    let output = std::process::Command::new("osascript")
        .args(["-e", "tell application \"System Events\" to unix id of first process whose frontmost is true"])
        .output()
        .expect("osascript failed");
    let pid: i32 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .expect("bad pid");

    println!("Focused app PID: {}", pid);
    let app = unsafe { AXUIElementCreateApplication(pid) };

    println!("\n=== App-level attributes ===");
    dump_attributes(app, 0);

    // Get focused window
    if let Some(window) = get_attr(app, "AXFocusedWindow") {
        let win_ref = window.as_CFTypeRef() as AXUIElementRef;
        println!("\n=== Window attributes ===");
        dump_attributes(win_ref, 0);

        // Recursively dump children (deeper than dump_tree)
        println!("\n=== Deep tree (up to depth 20, 100 elements) ===");
        let mut count = 0;
        dump_children(win_ref, 0, &mut count, 20, 100);
    } else {
        println!("No focused window found");
        // Try AXWindows
        if let Some(windows) = get_attr(app, "AXWindows") {
            let arr_ref = windows.as_CFTypeRef();
            if unsafe { core_foundation::array::CFArrayGetTypeID() }
                == unsafe { core_foundation::base::CFGetTypeID(arr_ref) }
            {
                let arr: CFArray<CFType> = unsafe {
                    CFArray::wrap_under_get_rule(arr_ref as core_foundation::array::CFArrayRef)
                };
                println!("Found {} windows via AXWindows", arr.len());
                if arr.len() > 0 {
                    if let Some(first) = arr.get(0) {
                        let win_ref = first.as_CFTypeRef() as AXUIElementRef;
                        println!("\n=== First window deep tree ===");
                        let mut count = 0;
                        dump_children(win_ref, 0, &mut count, 20, 100);
                    }
                }
            }
        }
    }

    unsafe { CFRelease(app as *const c_void) };
}

fn dump_children(el: AXUIElementRef, depth: usize, count: &mut usize, max_depth: usize, max_count: usize) {
    if depth >= max_depth || *count >= max_count {
        return;
    }
    *count += 1;

    let indent = "  ".repeat(depth);
    let role = get_string(el, "AXRole").unwrap_or_else(|| "?".into());
    let subrole = get_string(el, "AXSubrole").unwrap_or_default();
    let title = get_string(el, "AXTitle").unwrap_or_default();
    let desc = get_string(el, "AXDescription").unwrap_or_default();
    let value = get_string(el, "AXValue").unwrap_or_default();
    let role_desc = get_string(el, "AXRoleDescription").unwrap_or_default();

    let label = if !title.is_empty() {
        title
    } else if !desc.is_empty() {
        desc.clone()
    } else {
        "(none)".into()
    };

    let extra = if !subrole.is_empty() {
        format!(" sub={}", subrole)
    } else {
        String::new()
    };

    let val = if !value.is_empty() && value.len() < 50 {
        format!(" val=\"{}\"", value)
    } else if !value.is_empty() {
        format!(" val=\"{}...\"", &value[..47])
    } else {
        String::new()
    };

    println!(
        "{}[{}] {} \"{}\" rdesc=\"{}\"{}{} (desc=\"{}\")",
        indent, count, role, label, role_desc, extra, val, desc
    );

    // Get children
    if let Some(kids) = get_attr(el, "AXChildren") {
        let kids_ref = kids.as_CFTypeRef();
        if unsafe { core_foundation::array::CFArrayGetTypeID() }
            == unsafe { core_foundation::base::CFGetTypeID(kids_ref) }
        {
            let arr: CFArray<CFType> = unsafe {
                CFArray::wrap_under_get_rule(kids_ref as core_foundation::array::CFArrayRef)
            };
            for i in 0..arr.len() {
                if *count >= max_count {
                    break;
                }
                if let Some(child) = arr.get(i) {
                    dump_children(child.as_CFTypeRef() as AXUIElementRef, depth + 1, count, max_depth, max_count);
                }
            }
        }
    }
}

fn dump_attributes(el: AXUIElementRef, _depth: usize) {
    let mut names_ref: CFTypeRef = ptr::null();
    let err = unsafe { AXUIElementCopyAttributeNames(el, &mut names_ref) };
    if err != 0 || names_ref.is_null() {
        println!("  (no attributes)");
        return;
    }

    let arr: CFArray<CFType> = unsafe {
        CFArray::wrap_under_create_rule(names_ref as core_foundation::array::CFArrayRef)
    };

    for i in 0..arr.len() {
        if let Some(item) = arr.get(i) {
            let cf_ref = item.as_CFTypeRef();
            if unsafe { core_foundation::string::CFStringGetTypeID() }
                == unsafe { core_foundation::base::CFGetTypeID(cf_ref) }
            {
                let s: CFString = unsafe { CFString::wrap_under_get_rule(cf_ref as CFStringRef) };
                let name = s.to_string();
                let val = get_string(el, &name).unwrap_or_else(|| "(complex)".into());
                if val.len() < 80 {
                    println!("  {} = {}", name, val);
                } else {
                    println!("  {} = {}...", name, &val[..77]);
                }
            }
        }
    }
}

fn get_attr(el: AXUIElementRef, attr: &str) -> Option<CFType> {
    let attr_cf = CFString::new(attr);
    let mut value: CFTypeRef = ptr::null();
    let err = unsafe { AXUIElementCopyAttributeValue(el, attr_cf.as_concrete_TypeRef(), &mut value) };
    if err != 0 || value.is_null() {
        return None;
    }
    Some(unsafe { CFType::wrap_under_create_rule(value) })
}

fn get_string(el: AXUIElementRef, attr: &str) -> Option<String> {
    let cf = get_attr(el, attr)?;
    let cf_ref = cf.as_CFTypeRef();
    if unsafe { core_foundation::string::CFStringGetTypeID() }
        == unsafe { core_foundation::base::CFGetTypeID(cf_ref) }
    {
        let s: CFString = unsafe { CFString::wrap_under_get_rule(cf_ref as CFStringRef) };
        let r = s.to_string();
        if r.is_empty() { None } else { Some(r) }
    } else {
        None
    }
}
