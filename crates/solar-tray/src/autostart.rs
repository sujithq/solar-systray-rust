//! "Start at sign-in" through the per-user Run key.

use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RRF_RT_REG_SZ, RegCloseKey, RegDeleteKeyValueW, RegGetValueW,
    RegOpenKeyExW, RegSetValueExW,
};
use windows::core::{PCWSTR, w};

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const VALUE: PCWSTR = w!("Solar Systray");

fn command() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find this program's path: {e}"))?;
    Ok(format!("\"{}\"", exe.display()))
}

fn stored() -> Option<String> {
    let mut bytes = 0u32;
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE, RRF_RT_REG_SZ, None, None, Some(&mut bytes)) }
        .ok()
        .ok()?;
    let mut buffer = vec![0u16; (bytes as usize).div_ceil(2).max(1)];
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            VALUE,
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
    }
    .ok()
    .ok()?;
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..end]))
}

/// True when the Run key starts this executable (a stale path from a moved copy does not count).
pub fn is_enabled() -> bool {
    match (stored(), command()) {
        (Some(stored), Ok(expected)) => stored.to_lowercase() == expected.to_lowercase(),
        _ => false,
    }
}

pub fn set_enabled(enabled: bool) -> Result<(), String> {
    if !enabled {
        let result = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE) };
        return match result {
            r if r.is_ok() || r == ERROR_FILE_NOT_FOUND => Ok(()),
            r => Err(format!("cannot remove the sign-in entry: {}", r.to_hresult().message())),
        };
    }
    let command = command()?;
    let mut key = HKEY::default();
    unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, None, KEY_SET_VALUE, &mut key) }
        .ok()
        .map_err(|e| format!("cannot open the Run key: {}", e.message()))?;
    let wide: Vec<u16> = command.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes: Vec<u8> = wide.iter().flat_map(|c| c.to_le_bytes()).collect();
    let result = unsafe { RegSetValueExW(key, VALUE, None, REG_SZ, Some(&bytes)) };
    let _ = unsafe { RegCloseKey(key) };
    result.ok().map_err(|e| format!("cannot add the sign-in entry: {}", e.message()))
}
