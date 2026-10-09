//! "Start with Windows": a value in the current user's Run key that starts the app in the
//! notification area (`--minimized`). No administrator rights are needed.

pub const MINIMIZED_ARG: &str = "--minimized";

#[cfg(windows)]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const VALUE_NAME: &str = "CS2 Player Intel";

/// The command line Windows runs at sign-in.
pub fn command(exe: &std::path::Path) -> String {
    format!("\"{}\" {MINIMIZED_ARG}", exe.display())
}

#[cfg(windows)]
pub fn set(enabled: bool) -> Result<(), String> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
    let key = winreg::RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE).map_err(|e| e.to_string())?;
    if enabled {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        key.set_value(VALUE_NAME, &command(&exe)).map_err(|e| e.to_string())
    } else {
        match key.delete_value(VALUE_NAME) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.to_string()),
            _ => Ok(()),
        }
    }
}

#[cfg(not(windows))]
pub fn set(_enabled: bool) -> Result<(), String> {
    Err("Start with Windows is only available on Windows.".into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn command_quotes_the_path() {
        let path = std::path::Path::new(r"C:\Users\me\AppData\Local\CS2 Player Intel\cs2-player-intel.exe");
        assert_eq!(super::command(path), r#""C:\Users\me\AppData\Local\CS2 Player Intel\cs2-player-intel.exe" --minimized"#);
    }
}
