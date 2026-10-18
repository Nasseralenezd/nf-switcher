// Admin rights and the single instance.
//
// What needs admin (and so a UAC prompt): only the routing itself, i.e. opening the Windows Filtering Platform
// engine and adding or removing connection policies and block filters (engine/wfp.rs), plus switching Windows'
// "routepolicies" TCP/IP setting if the policies need it. Listing adapters, processes and connections, the UI and
// the saved settings all work without admin. NF Switcher starts asInvoker and asks for admin itself (runas), so a
// declined UAC prompt leaves a working, read-only app that says "Needs admin".
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcess, OpenMutexW, OpenProcessToken, SYNCHRONIZATION_SYNCHRONIZE};
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, FindWindowW, PostMessageW, RegisterWindowMessageW, ASFW_ANY, SW_SHOWNORMAL,
};

/// Machine-wide when elevated: the WFP rules and the routepolicies option are machine-wide too, so two users'
/// copies must not run at once. A normal (read-only) copy can't create Global\ objects and uses its session's.
pub const MUTEX: PCWSTR = w!("Global\\NFSwitcher.Instance");
pub const MUTEX_LOCAL: PCWSTR = w!("Local\\NFSwitcher.Instance");
pub const WINDOW_CLASS: PCWSTR = w!("NFSwitcherFlyout");

/// Is this process running with an elevated (admin) token?
pub fn is_elevated() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut e = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut e as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && e.TokenIsElevated != 0
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Relaunch {
    /// The elevated copy started; this one should exit.
    Started,
    /// The user said no to UAC.
    Declined,
    Failed(String),
}

/// Starts this exe again as admin (shows the UAC prompt) with `args`.
pub fn relaunch_as_admin(args: &[String]) -> Relaunch {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return Relaunch::Failed(e.to_string()),
    };
    let params = args.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ");
    let file = HSTRING::from(exe.as_os_str());
    let params = HSTRING::from(params);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    match unsafe { ShellExecuteExW(&mut info) } {
        Ok(()) => Relaunch::Started,
        // ERROR_CANCELLED: the user clicked No.
        Err(e) if e.code().0 as u32 == 0x800704C7 => Relaunch::Declined,
        Err(e) => Relaunch::Failed(e.message()),
    }
}

/// Quotes one command-line argument the way CommandLineToArgvW reads it back.
pub fn quote(a: &str) -> String {
    if !a.is_empty() && !a.contains([' ', '\t', '"']) {
        return a.to_string();
    }
    let mut out = String::from("\"");
    let mut slashes = 0;
    for c in a.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(slashes * 2));
    out.push('"');
    out
}

/// The message a second start sends to the running copy so it opens its flyout.
pub fn show_message() -> u32 {
    unsafe { RegisterWindowMessageW(w!("NFSwitcher.Show")) }
}

/// Is another NF Switcher running in this session? An elevated copy's mutex can't be opened from a normal process
/// (access denied), which still means it exists.
pub fn other_instance() -> bool {
    [MUTEX, MUTEX_LOCAL].into_iter().any(|name| match unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, name) } {
        Ok(h) => {
            unsafe {
                let _ = CloseHandle(h);
            }
            true
        }
        Err(e) => e.code() == ERROR_ACCESS_DENIED.to_hresult(),
    })
}

/// Asks the running copy to show itself (it allows this message through UIPI).
pub fn wake_other() {
    unsafe {
        if let Ok(hwnd) = FindWindowW(WINDOW_CLASS, PCWSTR::null()) {
            // This copy was just started by the user, so it may hand on the right to take the foreground.
            let _ = AllowSetForegroundWindow(ASFW_ANY);
            let _ = PostMessageW(Some(hwnd), show_message(), WPARAM(0), LPARAM(0));
        }
    }
}

/// Takes the single-instance mutex. None when another copy got it first. The handle is kept for the process'
/// life; Windows releases it when the process ends, crash or not.
pub fn take_instance() -> Option<HANDLE> {
    unsafe {
        let name = if is_elevated() { MUTEX } else { MUTEX_LOCAL };
        let h = CreateMutexW(None, false, name).ok()?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(h);
            return None;
        }
        Some(h)
    }
}

#[allow(dead_code)]
pub fn hwnd_none() -> HWND {
    HWND::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_round_trips_the_usual_cases() {
        assert_eq!(quote("--tray"), "--tray");
        assert_eq!(quote(r"C:\Program Files\x"), r#""C:\Program Files\x""#);
        assert_eq!(quote(r"C:\dir with space\"), r#""C:\dir with space\\""#);
        assert_eq!(quote(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote(""), r#""""#);
    }

    #[test]
    fn elevation_check_runs() {
        // Read-only; the result depends on how the tests were started.
        let _ = is_elevated();
    }
}
