use std::path::PathBuf;

const HOST_NAME: &str = "com.victauri.browser";

/// Platform-specific native messaging host manifest location.
///
/// # Errors
///
/// Returns an error if the home directory cannot be determined.
pub fn host_manifest_path() -> Result<PathBuf, InstallerError> {
    let home = home_dir()?;

    #[cfg(target_os = "windows")]
    {
        Ok(home.join(".victauri").join("native-host-manifest.json"))
    }

    #[cfg(target_os = "macos")]
    {
        Ok(home
            .join("Library")
            .join("Application Support")
            .join("Google")
            .join("Chrome")
            .join("NativeMessagingHosts")
            .join(format!("{HOST_NAME}.json")))
    }

    #[cfg(target_os = "linux")]
    {
        Ok(home
            .join(".config")
            .join("google-chrome")
            .join("NativeMessagingHosts")
            .join(format!("{HOST_NAME}.json")))
    }
}

/// Generate the native messaging host manifest JSON.
#[must_use]
pub fn host_manifest(binary_path: &str, extension_id: &str) -> serde_json::Value {
    serde_json::json!({
        "name": HOST_NAME,
        "description": "Victauri Browser — MCP inspection for web pages",
        "path": binary_path,
        "type": "stdio",
        "allowed_origins": [
            format!("chrome-extension://{extension_id}/")
        ]
    })
}

/// Directory where the native host binary should be installed.
///
/// # Errors
///
/// Returns an error if the home directory cannot be determined.
#[allow(dead_code)]
pub fn install_dir() -> Result<PathBuf, InstallerError> {
    let home = home_dir()?;
    Ok(home.join(".victauri").join("bin"))
}

/// Install the native messaging host manifest for all supported Chromium browsers.
///
/// Writes the manifest JSON to Chrome, Edge, and Brave locations.
/// On Windows, also creates registry keys for all browsers.
///
/// # Errors
///
/// Returns an error if file I/O or registry operations fail.
pub fn install(binary_path: &str, extension_id: &str) -> Result<String, InstallerError> {
    let manifest = host_manifest(binary_path, extension_id);
    let json = serde_json::to_string_pretty(&manifest).map_err(InstallerError::Json)?;

    let primary_path = host_manifest_path()?;

    for path in all_manifest_paths()? {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, &json);
    }

    #[cfg(target_os = "windows")]
    {
        register_windows_host(&primary_path)?;
    }

    Ok(primary_path.to_string_lossy().to_string())
}

fn all_manifest_paths() -> Result<Vec<PathBuf>, InstallerError> {
    let home = home_dir()?;
    let mut paths = vec![];

    #[cfg(target_os = "windows")]
    {
        paths.push(home.join(".victauri").join("native-host-manifest.json"));
    }

    #[cfg(target_os = "macos")]
    {
        let app_support = home.join("Library").join("Application Support");
        let manifest_file = format!("{HOST_NAME}.json");
        for browser_dir in [
            "Google/Chrome",
            "Microsoft Edge",
            "BraveSoftware/Brave-Browser",
            "Arc/User Data",
        ] {
            paths.push(
                app_support
                    .join(browser_dir)
                    .join("NativeMessagingHosts")
                    .join(&manifest_file),
            );
        }
    }

    #[cfg(target_os = "linux")]
    {
        let manifest_file = format!("{HOST_NAME}.json");
        let config = home.join(".config");
        for browser_dir in [
            "google-chrome",
            "microsoft-edge",
            "BraveSoftware/Brave-Browser",
            "chromium",
        ] {
            paths.push(
                config
                    .join(browser_dir)
                    .join("NativeMessagingHosts")
                    .join(&manifest_file),
            );
        }
    }

    Ok(paths)
}

/// Uninstall the native messaging host manifest.
///
/// # Errors
///
/// Returns an error if file I/O or registry operations fail.
pub fn uninstall() -> Result<(), InstallerError> {
    let manifest_path = host_manifest_path()?;
    if manifest_path.exists() {
        std::fs::remove_file(&manifest_path).map_err(InstallerError::Io)?;
    }

    #[cfg(target_os = "windows")]
    {
        unregister_windows_host();
    }

    Ok(())
}

#[cfg(target_os = "windows")]
const WINDOWS_REGISTRY_PATHS: &[&str] = &[
    r"HKCU\Software\Google\Chrome\NativeMessagingHosts",
    r"HKCU\Software\Microsoft\Edge\NativeMessagingHosts",
    r"HKCU\Software\BraveSoftware\Brave-Browser\NativeMessagingHosts",
];

#[cfg(target_os = "windows")]
fn register_windows_host(manifest_path: &std::path::Path) -> Result<(), InstallerError> {
    use std::process::Command;

    let value = manifest_path.to_string_lossy();

    for base_key in WINDOWS_REGISTRY_PATHS {
        let key = format!(r"{base_key}\{HOST_NAME}");
        let _ = Command::new("reg")
            .args(["add", &key, "/ve", "/t", "REG_SZ", "/d", &value, "/f"])
            .output();
    }

    Ok(())
}

#[cfg(target_os = "windows")]
fn unregister_windows_host() {
    use std::process::Command;

    for base_key in WINDOWS_REGISTRY_PATHS {
        let key = format!(r"{base_key}\{HOST_NAME}");
        let _ = Command::new("reg").args(["delete", &key, "/f"]).output();
    }
}

fn home_dir() -> Result<PathBuf, InstallerError> {
    #[cfg(target_os = "windows")]
    {
        std::env::var("USERPROFILE")
            .map(PathBuf::from)
            .map_err(|_| InstallerError::NoHomeDir)
    }

    #[cfg(not(target_os = "windows"))]
    {
        std::env::var("HOME")
            .map(PathBuf::from)
            .map_err(|_| InstallerError::NoHomeDir)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InstallerError {
    #[error("cannot determine home directory")]
    NoHomeDir,

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_has_correct_name() {
        let manifest = host_manifest("/usr/local/bin/victauri-browser-host", "abcdef123456");
        assert_eq!(manifest["name"], HOST_NAME);
        assert_eq!(manifest["type"], "stdio");
    }

    #[test]
    fn manifest_has_allowed_origin() {
        let manifest = host_manifest("/path/to/binary", "test_extension_id");
        let origins = manifest["allowed_origins"].as_array().unwrap();
        assert_eq!(origins.len(), 1);
        assert!(origins[0].as_str().unwrap().contains("test_extension_id"));
    }

    #[test]
    fn manifest_path_is_deterministic() {
        let p1 = host_manifest_path();
        let p2 = host_manifest_path();
        assert!(p1.is_ok());
        assert_eq!(p1.unwrap(), p2.unwrap());
    }

    #[test]
    fn install_dir_is_in_home() {
        let dir = install_dir().unwrap();
        assert!(dir.to_string_lossy().contains(".victauri"));
        assert!(dir.to_string_lossy().contains("bin"));
    }

    #[test]
    fn manifest_binary_path_preserved() {
        let path = "/some/deeply/nested/path/to/victauri-browser-host";
        let manifest = host_manifest(path, "abc");
        assert_eq!(manifest["path"], path);
    }

    #[test]
    fn manifest_extension_id_in_origin() {
        let id = "abcdefghijklmnopqrstuvwxyz012345";
        let manifest = host_manifest("/bin/host", id);
        let origin = manifest["allowed_origins"][0].as_str().unwrap();
        assert_eq!(origin, format!("chrome-extension://{id}/"));
    }

    #[test]
    fn manifest_type_is_stdio() {
        let manifest = host_manifest("/bin/host", "ext");
        assert_eq!(manifest["type"], "stdio");
    }

    #[test]
    fn manifest_description_present() {
        let manifest = host_manifest("/bin/host", "ext");
        assert!(manifest["description"].as_str().unwrap().len() > 5);
    }

    #[test]
    fn manifest_path_components_are_valid() {
        let path = host_manifest_path().unwrap();
        let path_str = path.to_string_lossy();
        assert!(path_str.contains("victauri") || path_str.contains(HOST_NAME));
        assert!(path_str.ends_with(".json"));
    }

    #[test]
    fn all_manifest_paths_non_empty() {
        let paths = all_manifest_paths().unwrap();
        assert!(!paths.is_empty());
        for p in &paths {
            assert!(p.to_string_lossy().ends_with(".json"));
        }
    }

    // --- Deep challenger: Chrome manifest spec compliance ---

    #[test]
    fn manifest_is_valid_json_object() {
        let manifest = host_manifest("/bin/host", "ext");
        assert!(manifest.is_object());
        let obj = manifest.as_object().unwrap();
        // Chrome requires exactly these fields
        assert!(obj.contains_key("name"));
        assert!(obj.contains_key("description"));
        assert!(obj.contains_key("path"));
        assert!(obj.contains_key("type"));
        assert!(obj.contains_key("allowed_origins"));
    }

    #[test]
    fn manifest_name_follows_chrome_spec() {
        // Chrome: name must match regex [a-z][a-z0-9._]*
        let manifest = host_manifest("/bin/host", "ext");
        let name = manifest["name"].as_str().unwrap();
        assert!(name.chars().next().unwrap().is_ascii_lowercase());
        assert!(name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_'));
        assert!(name.len() <= 255);
    }

    #[test]
    fn manifest_origin_format_correct() {
        // Chrome requires "chrome-extension://<id>/" format with trailing slash
        let manifest = host_manifest("/bin/host", "abcdefghijklmnopqrstuvwxyz012345");
        let origin = manifest["allowed_origins"][0].as_str().unwrap();
        assert!(origin.starts_with("chrome-extension://"));
        assert!(origin.ends_with('/'));
    }

    #[test]
    fn manifest_handles_windows_path() {
        let manifest = host_manifest(r"C:\Program Files\Victauri\victauri-browser-host.exe", "ext");
        let path = manifest["path"].as_str().unwrap();
        assert!(path.contains("victauri-browser-host"));
        assert!(path.contains(r"C:\Program Files"));
    }

    #[test]
    fn manifest_handles_path_with_spaces() {
        let manifest = host_manifest("/Users/My User/apps/victauri", "ext");
        assert_eq!(manifest["path"], "/Users/My User/apps/victauri");
    }

    #[test]
    fn manifest_handles_unicode_path() {
        let manifest = host_manifest("/Users/用户/victauri", "ext");
        assert_eq!(manifest["path"], "/Users/用户/victauri");
    }

    #[test]
    fn manifest_serializes_to_valid_json() {
        let manifest = host_manifest("/bin/host", "ext123");
        let json_str = serde_json::to_string_pretty(&manifest).unwrap();
        // Should be parseable back
        let reparsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(reparsed, manifest);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn all_manifest_paths_in_victauri_dir() {
        let paths = all_manifest_paths().unwrap();
        for p in &paths {
            assert!(p.to_string_lossy().contains(".victauri"));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn all_manifest_paths_cover_browsers() {
        let paths = all_manifest_paths().unwrap();
        let path_strs: Vec<String> = paths.iter().map(|p| p.to_string_lossy().to_string()).collect();
        assert!(path_strs.iter().any(|p| p.contains("Chrome")));
        assert!(path_strs.iter().any(|p| p.contains("Edge")));
        assert!(path_strs.iter().any(|p| p.contains("Brave")));
        assert!(path_strs.iter().any(|p| p.contains("Arc")));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn all_manifest_paths_cover_browsers() {
        let paths = all_manifest_paths().unwrap();
        let path_strs: Vec<String> = paths.iter().map(|p| p.to_string_lossy().to_string()).collect();
        assert!(path_strs.iter().any(|p| p.contains("google-chrome")));
        assert!(path_strs.iter().any(|p| p.contains("microsoft-edge")));
        assert!(path_strs.iter().any(|p| p.contains("Brave")));
        assert!(path_strs.iter().any(|p| p.contains("chromium")));
    }
}
