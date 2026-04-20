//! Dump the web page content area — skip Chrome UI, focus on AXWebArea children.

use core_foundation::array::CFArray;
use core_foundation::base::{CFType, TCFType};
use core_foundation::string::{CFString, CFStringRef};
use std::ffi::c_void;
use std::ptr;

type AXUIElementRef = *const c_void;
use core_foundation::base::CFTypeRef;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(el: AXUIElementRef, attr: CFStringRef, value: *mut CFTypeRef) -> i32;
    fn CFRelease(cf: *const c_void);
}

fn main() {
    let output = std::process::Command::new("osascript")
        .args(["-e", "tell application \"System Events\" to unix id of first process whose frontmost is true"])
        .output().expect("osascript failed");
    let pid: i32 = String::from_utf8_lossy(&output.stdout).trim().parse().expect("bad pid");

    let app = unsafe { AXUIElementCreateApplication(pid) };
    let window = get_attr(app, "AXFocusedWindow");
    let win_ref = window.as_ref().map(|w| w.as_CFTypeRef() as AXUIElementRef).unwrap_or(app);

    // Find AXWebArea elements (the page content boundary)
    println!("=== Searching for AXWebArea (page content) ===\n");
    let mut web_areas = Vec::new();
    find_web_areas(win_ref, 0, &mut web_areas, 15);

    if web_areas.is_empty() {
        println!("No AXWebArea found — not a browser/Electron app?");
    }

    for (i, wa) in web_areas.iter().enumerate() {
        let title = get_string(*wa, "AXTitle").unwrap_or_else(|| "(untitled)".into());
        let url = get_string(*wa, "AXURL");
        println!("--- WebArea #{}: \"{}\" ---", i + 1, title);
        if let Some(u) = &url { println!("    URL: {}", u); }
        println!();

        // Dump ALL children of this web area with full text
        let mut count = 0;
        dump_web_content(*wa, 0, &mut count, 20, 300);
        println!("\n    ({} elements in this web area)\n", count);
    }

    unsafe { CFRelease(app as *const c_void) };
}

fn find_web_areas(el: AXUIElementRef, depth: usize, results: &mut Vec<AXUIElementRef>, max_depth: usize) {
    if depth >= max_depth { return; }
    let role = get_string(el, "AXRole").unwrap_or_default();
    if role == "AXWebArea" {
        results.push(el);
        return; // Don't recurse into web area children here
    }
    if let Some(kids) = get_attr(el, "AXChildren") {
        let kids_ref = kids.as_CFTypeRef();
        if unsafe { core_foundation::array::CFArrayGetTypeID() } == unsafe { core_foundation::base::CFGetTypeID(kids_ref) } {
            let arr: CFArray<CFType> = unsafe { CFArray::wrap_under_get_rule(kids_ref as core_foundation::array::CFArrayRef) };
            for i in 0..arr.len() {
                if let Some(child) = arr.get(i) {
                    find_web_areas(child.as_CFTypeRef() as AXUIElementRef, depth + 1, results, max_depth);
                }
            }
        }
    }
}

fn dump_web_content(el: AXUIElementRef, depth: usize, count: &mut usize, max_depth: usize, max_count: usize) {
    if depth >= max_depth || *count >= max_count { return; }
    *count += 1;

    let role = get_string(el, "AXRole").unwrap_or_default();
    let title = get_string(el, "AXTitle").unwrap_or_default();
    let desc = get_string(el, "AXDescription").unwrap_or_default();
    let value = get_string(el, "AXValue").unwrap_or_default();
    let role_desc = get_string(el, "AXRoleDescription").unwrap_or_default();

    let text = if !title.is_empty() { &title }
        else if !value.is_empty() { &value }
        else if !desc.is_empty() { &desc }
        else { "" };

    // Only print elements that have content
    if !text.is_empty() || matches!(role.as_str(), "AXLink" | "AXButton" | "AXTextField" | "AXTextArea" | "AXImage" | "AXHeading") {
        let indent = "  ".repeat(depth.min(10));
        let display_text = if text.len() > 120 { format!("{}...", &text[..117]) } else { text.to_string() };
        println!("{}[{}] {} \"{}\" (rdesc={})", indent, count, role, display_text, role_desc);
    }

    if let Some(kids) = get_attr(el, "AXChildren") {
        let kids_ref = kids.as_CFTypeRef();
        if unsafe { core_foundation::array::CFArrayGetTypeID() } == unsafe { core_foundation::base::CFGetTypeID(kids_ref) } {
            let arr: CFArray<CFType> = unsafe { CFArray::wrap_under_get_rule(kids_ref as core_foundation::array::CFArrayRef) };
            for i in 0..arr.len() {
                if *count >= max_count { break; }
                if let Some(child) = arr.get(i) {
                    dump_web_content(child.as_CFTypeRef() as AXUIElementRef, depth + 1, count, max_depth, max_count);
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
    if unsafe { core_foundation::string::CFStringGetTypeID() } == unsafe { core_foundation::base::CFGetTypeID(cf_ref) } {
        let s: CFString = unsafe { CFString::wrap_under_get_rule(cf_ref as CFStringRef) };
        let r = s.to_string();
        if r.is_empty() { None } else { Some(r) }
    } else { None }
}
