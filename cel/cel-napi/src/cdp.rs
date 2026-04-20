use napi_derive::napi;

/// Install the LaunchAgent that enables CDP on all Electron apps.
/// Returns "installed" if newly installed, "already_installed" if already present.
#[napi]
pub fn cdp_setup_install() -> napi::Result<String> {
    match cel_cdp::install_cdp_launch_agent() {
        Ok(true) => Ok("installed".to_string()),
        Ok(false) => Ok("already_installed".to_string()),
        Err(e) => Err(napi::Error::from_reason(e)),
    }
}

/// Uninstall the CDP LaunchAgent.
/// Returns "uninstalled" or "not_installed".
#[napi]
pub fn cdp_setup_uninstall() -> napi::Result<String> {
    match cel_cdp::uninstall_cdp_launch_agent() {
        Ok(true) => Ok("uninstalled".to_string()),
        Ok(false) => Ok("not_installed".to_string()),
        Err(e) => Err(napi::Error::from_reason(e)),
    }
}

/// Check if the CDP LaunchAgent is installed.
#[napi]
pub fn cdp_is_setup() -> bool {
    cel_cdp::is_cdp_setup_installed()
}

/// Discover available CDP targets. Returns JSON array of CdpTarget.
#[napi]
pub fn cdp_discover_targets() -> napi::Result<String> {
    let targets = cel_cdp::discover_cdp_targets();
    serde_json::to_string(&targets).map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// Extract page content from the first available CDP target.
/// Returns JSON PageContent, or "null" if no CDP target is available.
#[napi]
pub async fn cdp_get_page_content() -> napi::Result<String> {
    let client = match cel_cdp::connect_to_focused_app().await {
        Some(c) => c,
        None => return Ok("null".to_string()),
    };

    match cel_cdp::extract_page_content(&client).await {
        Ok(content) => {
            serde_json::to_string(&content).map_err(|e| napi::Error::from_reason(e.to_string()))
        }
        Err(e) => Err(napi::Error::from_reason(format!(
            "CDP content extraction failed: {}",
            e
        ))),
    }
}

/// Get all cookies from the focused CDP target. Returns JSON array.
#[napi]
pub async fn cdp_get_cookies() -> napi::Result<String> {
    let client = match cel_cdp::connect_to_focused_app().await {
        Some(c) => c,
        None => return Ok("[]".to_string()),
    };

    match client.get_cookies().await {
        Ok(cookies) => serde_json::to_string(&cookies)
            .map_err(|e| napi::Error::from_reason(e.to_string())),
        Err(e) => Err(napi::Error::from_reason(e.to_string())),
    }
}

/// Get a localStorage value by key from the focused CDP target.
#[napi]
pub async fn cdp_get_local_storage(key: String) -> napi::Result<String> {
    let client = match cel_cdp::connect_to_focused_app().await {
        Some(c) => c,
        None => return Ok("null".to_string()),
    };

    match client.get_local_storage(&key).await {
        Ok(Some(val)) => Ok(val),
        Ok(None) => Ok("null".to_string()),
        Err(e) => Err(napi::Error::from_reason(e.to_string())),
    }
}

/// Get recent network requests (real HTTP data) from the focused CDP target.
/// Returns JSON array of HttpEvent.
#[napi]
pub async fn cdp_get_network_requests(limit: Option<u32>) -> napi::Result<String> {
    let client = match cel_cdp::connect_to_focused_app().await {
        Some(c) => c,
        None => return Ok("[]".to_string()),
    };

    match client.get_network_requests(limit.unwrap_or(20) as usize).await {
        Ok(events) => serde_json::to_string(&events)
            .map_err(|e| napi::Error::from_reason(e.to_string())),
        Err(e) => Err(napi::Error::from_reason(e.to_string())),
    }
}

/// Navigate the focused CDP target to a URL.
#[napi]
pub async fn cdp_navigate(url: String) -> napi::Result<()> {
    let client = match cel_cdp::connect_to_focused_app().await {
        Some(c) => c,
        None => return Err(napi::Error::from_reason("No CDP target available")),
    };

    client.navigate(&url).await
        .map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// Execute JavaScript in the focused browser tab via CDP Runtime.evaluate.
/// Returns the JSON-serialized result value, or "null" if no value.
#[napi]
pub async fn cdp_evaluate(expression: String) -> napi::Result<String> {
    let client = match cel_cdp::connect_to_focused_app().await {
        Some(c) => c,
        None => return Err(napi::Error::from_reason("No CDP target available. Is Chrome running with --remote-debugging-port?")),
    };

    let result = client.evaluate(&expression).await
        .map_err(|e| napi::Error::from_reason(format!("CDP evaluate failed: {}", e)))?;

    serde_json::to_string(&result)
        .map_err(|e| napi::Error::from_reason(e.to_string()))
}
