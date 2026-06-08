//! Cross-platform window-management types (WS2 of the Peekaboo-parity plan).
//!
//! The macOS implementation lives in `macos::perform_window_op` (AX-based);
//! other platforms get a stub via `lib.rs`. These types are platform-agnostic
//! so the contracts/cortex/napi layers can talk about window ops without a
//! per-OS dependency.

use serde::{Deserialize, Serialize};

/// Final geometry of a window after a [`WindowOp`], returned for
/// verify-by-readback receipts (matches Peekaboo's window-op JSON readback).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WindowGeom {
    /// Top-left x in global screen coordinates (points).
    pub x: f64,
    /// Top-left y in global screen coordinates (points).
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub minimized: bool,
    /// Window title, when the AX tree exposes one.
    pub title: Option<String>,
}

/// A window-management operation, applied to a target window resolved by app +
/// window index (index 0 = the app's frontmost window).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum WindowOp {
    /// Move the window's top-left to `(x, y)`.
    Move { x: f64, y: f64 },
    /// Resize the window to `width` × `height`.
    Resize { width: f64, height: f64 },
    /// Move + resize in one operation.
    SetBounds {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
    /// Minimize to the Dock.
    Minimize,
    /// Restore from minimized.
    Unminimize,
    /// Native zoom (green-button) toggle.
    Maximize,
    /// Raise the window within its app and make it the main window.
    Focus,
}

/// A Dock operation (WS6).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum DockOp {
    /// List Dock item titles.
    List,
    /// Launch / activate the Dock item titled `name` (AXPress).
    Launch { name: String },
    /// Open the Dock item's context menu (AXShowMenu).
    RightClick { name: String },
    /// Enable Dock auto-hide.
    Hide,
    /// Disable Dock auto-hide.
    Show,
}

/// Result of a [`DockOp`] (WS6). `items` is populated for `List`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct DockResult {
    pub items: Vec<String>,
}

/// A menu-bar-extra (system status item) operation (WS7).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum MenuExtraOp {
    /// List system menu-bar-extra titles.
    List,
    /// Click (open) the menu extra whose title contains `name` (case-insensitive).
    Click { name: String },
}

/// Result of a [`MenuExtraOp`] (WS7). `items` is populated for `List`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct MenuExtraResult {
    pub items: Vec<String>,
}
