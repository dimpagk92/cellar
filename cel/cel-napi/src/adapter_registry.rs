//! Adapter Registry — NAPI bindings for Rust-native adapter lifecycle.
//!
//! Exposes connect/disconnect/probe/get_elements/execute_action for adapters
//! that implement the `Adapter` trait from adapter-common.

use napi_derive::napi;
use std::collections::HashMap;

use adapter_common::{Adapter, AdapterInfo};

// ── Static adapter storage ─────────────────────────────────────────────────
// Uses tokio::sync::Mutex so the guard is Send-safe across .await points.

static ADAPTERS: std::sync::OnceLock<tokio::sync::Mutex<HashMap<String, Box<dyn Adapter>>>> =
    std::sync::OnceLock::new();

fn adapters() -> &'static tokio::sync::Mutex<HashMap<String, Box<dyn Adapter>>> {
    ADAPTERS.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()))
}

// ── Factory: create adapter by name ────────────────────────────────────────

fn create_adapter(name: &str) -> napi::Result<Box<dyn Adapter>> {
    match name {
        // Future: "excel" => Ok(Box::new(excel_adapter::ExcelAdapter::new())),
        // Future: "sap-gui" => Ok(Box::new(sap_gui_adapter::SapGuiAdapter::new())),
        // Future: "bloomberg" => Ok(Box::new(bloomberg_adapter::BloombergAdapter::new())),
        // Future: "metatrader" => Ok(Box::new(metatrader_adapter::MetaTraderAdapter::new())),
        _ => Err(napi::Error::from_reason(format!(
            "Unknown adapter: \"{name}\". Available: excel, sap-gui, bloomberg, metatrader"
        ))),
    }
}

// ── NAPI exports ───────────────────────────────────────────────────────────

/// Register a Rust-native adapter by name.
#[napi]
pub async fn register_adapter(name: String) -> napi::Result<()> {
    let adapter = create_adapter(&name)?;
    let mut map = adapters().lock().await;
    map.insert(name, adapter);
    Ok(())
}

/// Connect a registered adapter to its target application.
#[napi]
pub async fn connect_adapter(name: String) -> napi::Result<()> {
    let mut map = adapters().lock().await;
    let adapter = map.get_mut(&name).ok_or_else(|| {
        napi::Error::from_reason(format!("Adapter \"{name}\" not registered"))
    })?;
    adapter.connect().await.map_err(|e| {
        napi::Error::from_reason(format!("Connect failed: {e}"))
    })
}

/// Disconnect a registered adapter.
#[napi]
pub async fn disconnect_adapter(name: String) -> napi::Result<()> {
    let mut map = adapters().lock().await;
    let adapter = map.get_mut(&name).ok_or_else(|| {
        napi::Error::from_reason(format!("Adapter \"{name}\" not registered"))
    })?;
    adapter.disconnect().await.map_err(|e| {
        napi::Error::from_reason(format!("Disconnect failed: {e}"))
    })
}

/// Check if a registered adapter's target app is running.
#[napi]
pub async fn probe_adapter(name: String) -> napi::Result<bool> {
    let map = adapters().lock().await;
    let adapter = map.get(&name).ok_or_else(|| {
        napi::Error::from_reason(format!("Adapter \"{name}\" not registered"))
    })?;
    Ok(adapter.is_available().await)
}

/// Get context elements from a registered adapter (JSON-serialized).
#[napi]
pub async fn adapter_get_elements(name: String) -> napi::Result<String> {
    let map = adapters().lock().await;
    let adapter = map.get(&name).ok_or_else(|| {
        napi::Error::from_reason(format!("Adapter \"{name}\" not registered"))
    })?;
    let elements = adapter.get_elements().await.map_err(|e| {
        napi::Error::from_reason(format!("get_elements failed: {e}"))
    })?;
    serde_json::to_string(&elements).map_err(|e| {
        napi::Error::from_reason(format!("JSON serialization failed: {e}"))
    })
}

/// Execute a named action on a registered adapter (JSON in/out).
#[napi]
pub async fn adapter_execute_action(
    name: String,
    action: String,
    params: String,
) -> napi::Result<String> {
    let map = adapters().lock().await;
    let adapter = map.get(&name).ok_or_else(|| {
        napi::Error::from_reason(format!("Adapter \"{name}\" not registered"))
    })?;
    let params_val: serde_json::Value = serde_json::from_str(&params).map_err(|e| {
        napi::Error::from_reason(format!("Invalid params JSON: {e}"))
    })?;
    let result = adapter.execute_action(&action, params_val).await.map_err(|e| {
        napi::Error::from_reason(format!("execute_action failed: {e}"))
    })?;
    serde_json::to_string(&result).map_err(|e| {
        napi::Error::from_reason(format!("JSON serialization failed: {e}"))
    })
}

/// Get adapter info (JSON-serialized).
#[napi]
pub async fn adapter_info(name: String) -> napi::Result<String> {
    let map = adapters().lock().await;
    let adapter = map.get(&name).ok_or_else(|| {
        napi::Error::from_reason(format!("Adapter \"{name}\" not registered"))
    })?;
    let info: AdapterInfo = adapter.info();
    serde_json::to_string(&info).map_err(|e| {
        napi::Error::from_reason(format!("JSON serialization failed: {e}"))
    })
}
