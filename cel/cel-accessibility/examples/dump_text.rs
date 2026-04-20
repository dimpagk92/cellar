//! Dump ALL elements with text content — check what AX actually gives us for web views.
//! Focuses on: AXValue, AXTitle, AXDescription, AXRoleDescription, AXHelp

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
    fn CFRelease(cf: *const c_void);
}

fn main() {
    let output = std::process::Command::new("osascript")
        .args(["-e", "tell application \"System Events\" to unix id of first process whose frontmost is true"])
        .output()
        .expect("osascript failed");
    let pid: i32 = String::from_utf8_lossy(&output.stdout).trim().parse().expect("bad pid");

    let app = unsafe { AXUIElementCreateApplication(pid) };
    let window = get_attr(app, "AXFocusedWindow");
    let win_ref = window.as_ref().map(|w| w.as_CFTypeRef() as AXUIElementRef).unwrap_or(app);

    println!("=== Elements with text content (AXValue, AXTitle, AXDescription) ===\n");
    let mut count = 0;
    let mut text_count = 0;
    dump_text_elements(win_ref, 0, &mut count, &mut text_count, 25, 500);
    println!("\n--- Total: {} elements scanned, {} with text content ---", count, text_count);

    unsafe { CFRelease(app as *const c_void) };
}

fn dump_text_elements(el: AXUIElementRef, depth: usize, count: &mut usize, text_count: &mut usize, max_depth: usize, max_count: usize) {
    if depth >= max_depth || *count >= max_count { return; }
    *count += 1;

    let role = get_string(el, "AXRole").unwrap_or_default();
    let title = get_string(el, "AXTitle");
    let desc = get_string(el, "AXDescription");
    let value = get_string(el, "AXValue");
    let help = get_string(el, "AXHelp");
    let role_desc = get_string(el, "AXRoleDescription");

    let has_text = title.is_some() || desc.is_some() || value.is_some() || help.is_some();

    if has_text {
        *text_count += 1;
        let indent = "  ".repeat(depth.min(8));
        println!("{}[{}] {}", indent, count, role);
        if let Some(t) = &title {
            let t = if t.len() > 100 { format!("{}...", &t[..97]) } else { t.clone() };
            println!("{}  title: \"{}\"", indent, t);
        }
        if let Some(d) = &desc {
            let d = if d.len() > 100 { format!("{}...", &d[..97]) } else { d.clone() };
            println!("{}  desc:  \"{}\"", indent, d);
        }
        if let Some(v) = &value {
            let v = if v.len() > 200 { format!("{}...", &v[..197]) } else { v.clone() };
            println!("{}  value: \"{}\"", indent, v);
        }
        if let Some(h) = &help {
            let h = if h.len() > 100 { format!("{}...", &h[..97]) } else { h.clone() };
            println!("{}  help:  \"{}\"", indent, h);
        }
        if let Some(r) = &role_desc {
            println!("{}  rdesc: \"{}\"", indent, r);
        }
    }

    // Recurse children
    if let Some(kids) = get_attr(el, "AXChildren") {
        let kids_ref = kids.as_CFTypeRef();
        if unsafe { core_foundation::array::CFArrayGetTypeID() }
            == unsafe { core_foundation::base::CFGetTypeID(kids_ref) }
        {
            let arr: CFArray<CFType> = unsafe {
                CFArray::wrap_under_get_rule(kids_ref as core_foundation::array::CFArrayRef)
            };
            for i in 0..arr.len() {
                if *count >= max_count { break; }
                if let Some(child) = arr.get(i) {
                    dump_text_elements(child.as_CFTypeRef() as AXUIElementRef, depth + 1, count, text_count, max_depth, max_count);
                }
            }
        }
    }
}

fn get_attr(el: AXUIElementRef, attr: &str) -> Option<CFType> {
    let attr_cf = CFString::new(attr);
    let mut value: CFTypeRef = ptr::null();
    let err = unsafe { AXUIElementCopyAttributeValue(el, attr_cf.as_concrete_TypeRef(), &mut value) };
    if err != 0 || value.is_null() { return None; }
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
