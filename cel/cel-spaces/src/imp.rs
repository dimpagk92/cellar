//! Real SkyLight FFI implementation (macOS + `spaces` feature).
//!
//! ⚠️ Private API. The `CGS*` signatures are reverse-engineered (the
//! yabai / Amethyst lineage) and stable across recent macOS, but Apple can
//! change them. Every call is defensive: a zero connection id, a null result,
//! or a shape/type mismatch maps to an error or fewer entries — never a panic.
//! The undocumented `CGSCopyManagedDisplaySpaces` payload is parsed with raw,
//! type-id-checked CoreFoundation accessors so a wrong key or type yields
//! `None` rather than misinterpreting memory.

use crate::{Result, SpaceInfo, SpacesError};
use core_foundation::array::{CFArray, CFArrayRef};
use core_foundation::base::{CFType, TCFType};
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use std::os::raw::c_void;

#[allow(non_camel_case_types)]
type CGSConnectionID = i32;
type CGSSpaceID = u64;

#[link(name = "SkyLight", kind = "framework")]
extern "C" {
    fn CGSMainConnectionID() -> CGSConnectionID;
    fn CGSGetActiveSpace(cid: CGSConnectionID) -> CGSSpaceID;
    fn CGSCopyManagedDisplaySpaces(cid: CGSConnectionID) -> CFArrayRef;
    fn CGSManagedDisplaySetCurrentSpace(
        cid: CGSConnectionID,
        display: CFStringRef,
        space: CGSSpaceID,
    );
    fn CGSMoveWindowsToManagedSpace(cid: CGSConnectionID, windows: CFArrayRef, space: CGSSpaceID);
}

// Public, stable CoreFoundation accessors used to walk the undocumented
// CGS payload without core-foundation's typed wrappers (which require
// ConcreteCFType for nested untyped dictionaries/arrays).
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFArrayGetCount(arr: *const c_void) -> isize;
    fn CFArrayGetValueAtIndex(arr: *const c_void, idx: isize) -> *const c_void;
    fn CFDictionaryGetValue(dict: *const c_void, key: *const c_void) -> *const c_void;
    fn CFGetTypeID(cf: *const c_void) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFNumberGetTypeID() -> usize;
}

fn connection() -> Result<CGSConnectionID> {
    let cid = unsafe { CGSMainConnectionID() };
    if cid == 0 {
        return Err(SpacesError::Unavailable);
    }
    Ok(cid)
}

/// True iff a SkyLight connection is obtainable on this system.
pub fn spaces_available() -> bool {
    unsafe { CGSMainConnectionID() != 0 }
}

/// The active space id on the main display.
pub fn active_space() -> Result<u64> {
    let cid = connection()?;
    Ok(unsafe { CGSGetActiveSpace(cid) })
}

/// Switch the given display (UUID from [`list_spaces`]) to the given space.
pub fn switch_to_space(display_uuid: &str, space_id: u64) -> Result<()> {
    let cid = connection()?;
    let display = CFString::new(display_uuid);
    unsafe { CGSManagedDisplaySetCurrentSpace(cid, display.as_concrete_TypeRef(), space_id) };
    Ok(())
}

/// Move a window (by its CoreGraphics window id) to the given space.
pub fn move_window_to_space(window_id: u32, space_id: u64) -> Result<()> {
    let cid = connection()?;
    let ids = [CFNumber::from(window_id as i64)];
    let windows = CFArray::from_CFTypes(&ids);
    unsafe { CGSMoveWindowsToManagedSpace(cid, windows.as_concrete_TypeRef(), space_id) };
    Ok(())
}

/// Enumerate the managed spaces across all displays.
///
/// ⚠️ Parses the undocumented `CGSCopyManagedDisplaySpaces` payload — an array
/// of per-display dictionaries with `"Display Identifier"`, `"Current Space"`,
/// and `"Spaces"` (each space dict carrying `"id64"`). Type-id-checked and
/// defensive: a mismatch yields fewer / zero entries. Most version-fragile
/// call in the crate.
pub fn list_spaces() -> Result<Vec<SpaceInfo>> {
    let cid = connection()?;
    let arr_ref = unsafe { CGSCopyManagedDisplaySpaces(cid) };
    if arr_ref.is_null() {
        return Err(SpacesError::CallFailed(
            "CGSCopyManagedDisplaySpaces returned null".into(),
        ));
    }
    // Follows the Create Rule — wrapping it gives us ownership + auto-release.
    let displays: CFArray<CFType> = unsafe { CFArray::wrap_under_create_rule(arr_ref) };

    let mut out = Vec::new();
    for display in displays.iter() {
        let dptr = display.as_CFTypeRef() as *const c_void;
        let display_uuid =
            unsafe { cf_string_at(dptr, "Display Identifier") }.unwrap_or_else(|| "Main".into());
        let current_id = unsafe {
            let cs = cf_dict_get(dptr, "Current Space");
            cf_u64_at(cs, "id64")
        };
        let spaces_ptr = unsafe { cf_dict_get(dptr, "Spaces") };
        for sptr in unsafe { cf_array_items(spaces_ptr) } {
            if let Some(id) = unsafe { cf_u64_at(sptr, "id64") } {
                out.push(SpaceInfo {
                    space_id: id,
                    display_uuid: display_uuid.clone(),
                    is_current: Some(id) == current_id,
                });
            }
        }
    }
    Ok(out)
}

// ── raw CF helpers (all null/type-checked; safe to call on any pointer) ──

unsafe fn cf_dict_get(dict: *const c_void, key: &str) -> *const c_void {
    if dict.is_null() {
        return std::ptr::null();
    }
    let k = CFString::new(key);
    CFDictionaryGetValue(dict, k.as_concrete_TypeRef() as *const c_void)
}

unsafe fn cf_array_items(arr: *const c_void) -> Vec<*const c_void> {
    if arr.is_null() {
        return Vec::new();
    }
    let n = CFArrayGetCount(arr);
    (0..n)
        .map(|i| CFArrayGetValueAtIndex(arr, i))
        .filter(|p| !p.is_null())
        .collect()
}

unsafe fn cf_string_at(dict: *const c_void, key: &str) -> Option<String> {
    let v = cf_dict_get(dict, key);
    if v.is_null() || CFGetTypeID(v) != CFStringGetTypeID() {
        return None;
    }
    Some(CFString::wrap_under_get_rule(v as CFStringRef).to_string())
}

unsafe fn cf_u64_at(dict: *const c_void, key: &str) -> Option<u64> {
    let v = cf_dict_get(dict, key);
    if v.is_null() || CFGetTypeID(v) != CFNumberGetTypeID() {
        return None;
    }
    CFNumber::wrap_under_get_rule(v as *const _ as _)
        .to_i64()
        .map(|i| i as u64)
}
