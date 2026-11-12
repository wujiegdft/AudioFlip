use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::*;
use windows::core::*;

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const VALUE: PCWSTR = w!("AudioFlip");

fn command() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(format!("\"{}\"", exe.display()))
}

pub fn is_enabled() -> bool {
    let mut buf = [0u16; 1024];
    let mut len = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            VALUE,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as _),
            Some(&mut len),
        )
    };
    if r != ERROR_SUCCESS {
        return false;
    }
    let n = (len as usize / 2).saturating_sub(1);
    let val = String::from_utf16_lossy(&buf[..n]);
    command().is_some_and(|c| c.eq_ignore_ascii_case(&val))
}

pub fn set(enable: bool) -> bool {
    unsafe {
        if enable {
            let Some(cmd) = command() else { return false };
            let wide: Vec<u16> = cmd.encode_utf16().chain(Some(0)).collect();
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                VALUE,
                REG_SZ.0,
                Some(wide.as_ptr() as _),
                (wide.len() * 2) as u32,
            ) == ERROR_SUCCESS
        } else {
            RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE) == ERROR_SUCCESS
        }
    }
}
