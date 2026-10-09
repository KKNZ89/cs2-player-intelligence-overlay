//! Whether CS2 runs (a read-only process list check) and launching it through Steam, so normal launch
//! options, updates and anti-cheat start as usual. The game process itself is never touched.

pub const LAUNCH_URL: &str = "steam://rungameid/730";

#[cfg(windows)]
pub fn is_cs2_running() -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
    // SAFETY: the snapshot handle is closed before returning; entries are plain structs sized for the API.
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return false };
        let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut found = false;
        let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
        while more {
            let length = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            if String::from_utf16_lossy(&entry.szExeFile[..length]).eq_ignore_ascii_case("cs2.exe") {
                found = true;
                break;
            }
            more = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
        found
    }
}

#[cfg(not(windows))]
pub fn is_cs2_running() -> bool {
    false
}

#[cfg(test)]
mod tests {
    #[test]
    fn process_check_does_not_fail() {
        // The answer depends on the machine; the check itself must complete without error.
        let _ = super::is_cs2_running();
    }
}
