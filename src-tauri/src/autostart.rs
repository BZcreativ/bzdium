/// Applies the `startWithWindows` setting via the registry Run key.
#[cfg(windows)]
pub fn apply(enabled: bool) -> Result<(), String> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu
        .create_subkey(r"Software\Microsoft\Windows\CurrentVersion\Run")
        .map_err(|e| e.to_string())?;
    if enabled {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        key.set_value("bzdium", &format!("\"{}\"", exe.display()))
            .map_err(|e| e.to_string())?;
    } else {
        // Ignore "value not found".
        let _ = key.delete_value("bzdium");
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn apply(_enabled: bool) -> Result<(), String> {
    Ok(())
}
