// Installing and uninstalling NF Switcher (used by bin/setup.rs). For all users: the app goes to
// C:\Program Files\NF Switcher, a Start menu entry (and optionally a desktop one) is added, Settings › Apps gets an
// entry with an uninstaller, and "Start with Windows" is a sign-in task (autostart.rs).
//
// The sandbox layout (--sandbox DIR) puts everything under one folder and writes the registry entry and the task as
// files there instead, so the whole flow can be tested without changing the PC.
use crate::autostart;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use windows::core::{Interface, HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, WAIT_OBJECT_0, WPARAM};
use windows::Win32::System::Com::{CoCreateInstance, IPersistFile, CLSCTX_INPROC_SERVER};
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowThreadProcessId, PostMessageW, WM_CLOSE};

pub const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\NF Switcher";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub struct Layout {
    /// The install folder.
    pub dir: PathBuf,
    pub start_menu: PathBuf,
    pub desktop: PathBuf,
    /// NF Switcher's settings folder (%ProgramData%\NF Switcher).
    pub data: PathBuf,
    /// Sandbox: the registry entry and the task go here as files, nothing touches Windows.
    pub sandbox: Option<PathBuf>,
}

fn env_path(var: &str, fallback: &str) -> PathBuf {
    PathBuf::from(std::env::var(var).unwrap_or_else(|_| fallback.into()))
}

impl Layout {
    pub fn default_dir() -> PathBuf {
        env_path("ProgramFiles", r"C:\Program Files").join("NF Switcher")
    }

    pub fn real(dir: PathBuf) -> Layout {
        Layout {
            dir,
            start_menu: env_path("ProgramData", r"C:\ProgramData").join(r"Microsoft\Windows\Start Menu\Programs\NF Switcher.lnk"),
            desktop: env_path("PUBLIC", r"C:\Users\Public").join(r"Desktop\NF Switcher.lnk"),
            data: crate::secure::machine_dir(),
            sandbox: None,
        }
    }

    pub fn sandbox(root: &Path) -> Layout {
        Layout {
            dir: root.join(r"Program Files\NF Switcher"),
            start_menu: root.join(r"Start Menu\NF Switcher.lnk"),
            desktop: root.join(r"Desktop\NF Switcher.lnk"),
            data: root.join(r"ProgramData\NF Switcher"),
            sandbox: Some(root.join("system")),
        }
    }

    pub fn exe(&self) -> PathBuf {
        self.dir.join("nf-switcher.exe")
    }

    pub fn uninstaller(&self) -> PathBuf {
        self.dir.join("uninstall.exe")
    }
}

pub struct Options {
    pub autostart: bool,
    pub desktop: bool,
    /// The user whose sign-in starts NF Switcher (DOMAIN\name).
    pub user: String,
}

/// What the installed copy is: its folder and version, from Settings › Apps' entry.
pub fn installed() -> Option<(PathBuf, String)> {
    let dir = reg_read(UNINSTALL_KEY, "InstallLocation")?;
    let ver = reg_read(UNINSTALL_KEY, "DisplayVersion").unwrap_or_default();
    Some((PathBuf::from(dir), ver))
}

fn reg_read(key: &str, name: &str) -> Option<String> {
    let mut buf = vec![0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            &HSTRING::from(key),
            &HSTRING::from(name),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
    };
    if r.is_err() {
        return None;
    }
    let n = (size as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buf[..n.min(buf.len())]))
}

/// The values of Settings › Apps' entry.
fn uninstall_entry(l: &Layout, size_kb: u32) -> Vec<(&'static str, Value)> {
    let dir = l.dir.to_string_lossy().into_owned();
    let un = l.uninstaller().to_string_lossy().into_owned();
    vec![
        ("DisplayName", Value::Str("NF Switcher".into())),
        ("DisplayVersion", Value::Str(env!("CARGO_PKG_VERSION").into())),
        ("Publisher", Value::Str("NF".into())),
        ("DisplayIcon", Value::Str(format!("{},0", l.exe().to_string_lossy()))),
        ("InstallLocation", Value::Str(dir)),
        ("UninstallString", Value::Str(format!("\"{un}\" --uninstall"))),
        ("QuietUninstallString", Value::Str(format!("\"{un}\" --uninstall --quiet"))),
        ("URLInfoAbout", Value::Str(format!("https://github.com/{}", crate::update::REPO))),
        ("EstimatedSize", Value::Dword(size_kb)),
        ("NoModify", Value::Dword(1)),
        ("NoRepair", Value::Dword(1)),
    ]
}

#[derive(Debug, PartialEq)]
pub enum Value {
    Str(String),
    Dword(u32),
}

fn reg_write(key: &str, values: &[(&str, Value)]) -> Result<(), String> {
    unsafe {
        let mut h = HKEY::default();
        let r = RegCreateKeyExW(HKEY_LOCAL_MACHINE, &HSTRING::from(key), None, PCWSTR::null(), REG_OPTION_NON_VOLATILE, KEY_WRITE, None, &mut h, None);
        if r.is_err() {
            return Err(format!("registering in Settings › Apps failed ({})", r.0));
        }
        for (name, v) in values {
            let r = match v {
                Value::Str(s) => {
                    let w: Vec<u16> = s.encode_utf16().chain(Some(0)).collect();
                    let bytes = std::slice::from_raw_parts(w.as_ptr() as *const u8, w.len() * 2);
                    RegSetValueExW(h, &HSTRING::from(*name), None, REG_SZ, Some(bytes))
                }
                Value::Dword(d) => RegSetValueExW(h, &HSTRING::from(*name), None, REG_DWORD, Some(&d.to_le_bytes())),
            };
            if r.is_err() {
                let _ = RegCloseKey(h);
                return Err(format!("writing {name} failed ({})", r.0));
            }
        }
        let _ = RegCloseKey(h);
    }
    Ok(())
}

fn shortcut(lnk: &Path, target: &Path, args: &str) -> Result<(), String> {
    if let Some(d) = lnk.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| e.message())?;
        link.SetPath(&HSTRING::from(target.as_os_str())).map_err(|e| e.message())?;
        link.SetArguments(&HSTRING::from(args)).map_err(|e| e.message())?;
        if let Some(d) = target.parent() {
            link.SetWorkingDirectory(&HSTRING::from(d.as_os_str())).map_err(|e| e.message())?;
        }
        link.SetDescription(&HSTRING::from("Pick which apps use Wi-Fi")).map_err(|e| e.message())?;
        link.SetIconLocation(&HSTRING::from(target.as_os_str()), 0).map_err(|e| e.message())?;
        let file: IPersistFile = link.cast().map_err(|e| e.message())?;
        file.Save(&HSTRING::from(lnk.as_os_str()), true).map_err(|e| e.message())?;
    }
    Ok(())
}

/// Asks a running NF Switcher to quit (it takes its rules out of Windows as it goes), and waits for it. If it
/// doesn't go within 10 s it's stopped; Windows then removes its rules itself.
pub fn close_running() {
    unsafe {
        let Ok(hwnd) = FindWindowW(crate::elevate::WINDOW_CLASS, PCWSTR::null()) else { return };
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let proc = OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, false, pid).ok();
        let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        if let Some(p) = proc {
            if WaitForSingleObject(p, 10_000) != WAIT_OBJECT_0 {
                let _ = TerminateProcess(p, 1);
                let _ = WaitForSingleObject(p, 5_000);
            }
            let _ = CloseHandle(p);
        }
        let _: HWND = hwnd;
    }
}

/// The steps the window shows, in order.
pub const INSTALL_STEPS: [&str; 4] = ["Copy NF Switcher", "Add to Start and Settings › Apps", "Set up \"Start with Windows\"", "Done"];
pub const UNINSTALL_STEPS: [&str; 4] = ["Close NF Switcher", "Remove \"Start with Windows\"", "Remove shortcuts and the Apps entry", "Remove the files"];

/// Installs (or updates) into `l`. `step(i)` is called as each step starts.
pub fn install(l: &Layout, o: &Options, app: &[u8], setup_exe: &Path, step: &mut dyn FnMut(usize)) -> Result<(), String> {
    step(0);
    if l.sandbox.is_none() {
        close_running();
        std::fs::create_dir_all(&l.dir).map_err(|e| format!("creating {}: {e}", l.dir.display()))?;
        // Admin-only, whatever folder was picked: the app runs elevated (and at sign-in), so normal programs must
        // not be able to replace it.
        crate::secure::prepare(&l.dir)?;
        crate::secure::prepare(&l.data)?;
    } else {
        std::fs::create_dir_all(&l.dir).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&l.data).map_err(|e| e.to_string())?;
    }
    let tmp = l.dir.join("nf-switcher.exe.new");
    std::fs::write(&tmp, app).map_err(|e| format!("writing NF Switcher: {e}"))?;
    std::fs::rename(&tmp, l.exe()).map_err(|e| format!("replacing NF Switcher (is it still running?): {e}"))?;
    let same = std::fs::canonicalize(setup_exe).ok() == std::fs::canonicalize(l.uninstaller()).ok();
    if !same {
        std::fs::copy(setup_exe, l.uninstaller()).map_err(|e| format!("copying the uninstaller: {e}"))?;
    }

    step(1);
    shortcut(&l.start_menu, &l.exe(), "")?;
    if o.desktop {
        shortcut(&l.desktop, &l.exe(), "")?;
    } else {
        let _ = std::fs::remove_file(&l.desktop);
    }
    let size_kb = ((app.len() + std::fs::metadata(l.uninstaller()).map(|m| m.len() as usize).unwrap_or(0)) / 1024) as u32;
    let entry = uninstall_entry(l, size_kb);
    match &l.sandbox {
        None => reg_write(UNINSTALL_KEY, &entry)?,
        Some(sys) => {
            std::fs::create_dir_all(sys).map_err(|e| e.to_string())?;
            let text: Vec<String> = entry.iter().map(|(k, v)| format!("{k} = {v:?}")).collect();
            std::fs::write(sys.join("uninstall-entry.txt"), text.join("\n")).map_err(|e| e.to_string())?;
        }
    }

    step(2);
    match (&l.sandbox, o.autostart) {
        (None, true) => autostart::create(&l.exe(), &o.user, &l.data)?,
        (None, false) => autostart::delete()?,
        (Some(sys), true) => std::fs::write(sys.join("task.xml"), autostart::xml(&l.exe(), &o.user)).map_err(|e| e.to_string())?,
        (Some(sys), false) => {
            let _ = std::fs::remove_file(sys.join("task.xml"));
        }
    }
    step(3);
    Ok(())
}

/// Uninstalls from `l`. The folder itself (with this uninstaller in it) goes a moment after we exit.
pub fn uninstall(l: &Layout, remove_settings: bool, step: &mut dyn FnMut(usize)) -> Result<(), String> {
    step(0);
    if l.sandbox.is_none() {
        close_running();
        // If NF Switcher was stopped hard, Windows' routepolicies option may still be on: put it back now.
        use crate::engine::wfp::RoutePolicies;
        crate::engine::routepolicies::Netsh::new().release();
    }

    step(1);
    match &l.sandbox {
        None => autostart::delete()?,
        Some(sys) => {
            let _ = std::fs::remove_file(sys.join("task.xml"));
        }
    }

    step(2);
    let _ = std::fs::remove_file(&l.start_menu);
    let _ = std::fs::remove_file(&l.desktop);
    match &l.sandbox {
        None => unsafe {
            let _ = RegDeleteTreeW(HKEY_LOCAL_MACHINE, &HSTRING::from(UNINSTALL_KEY));
            let _ = RegDeleteKeyW(HKEY_LOCAL_MACHINE, &HSTRING::from(UNINSTALL_KEY));
        },
        Some(sys) => {
            let _ = std::fs::remove_file(sys.join("uninstall-entry.txt"));
        }
    }

    step(3);
    let me = std::env::current_exe().ok().and_then(|p| std::fs::canonicalize(p).ok());
    let running_inside = me.as_ref().is_some_and(|m| std::fs::canonicalize(&l.dir).ok().is_some_and(|d| m.starts_with(d)));
    let _ = std::fs::remove_file(l.exe());
    let mut later = Vec::new();
    if running_inside {
        later.push(l.dir.clone());
    } else {
        std::fs::remove_dir_all(&l.dir).map_err(|e| format!("removing {}: {e}", l.dir.display()))?;
    }
    if remove_settings {
        if std::fs::remove_dir_all(&l.data).is_err() {
            later.push(l.data.clone());
        }
    }
    if let Ok(mut p) = PENDING.lock() {
        p.extend(later);
    }
    Ok(())
}

/// Folders this exe can't delete while it runs (its own folder): removed by remove_after_exit().
static PENDING: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// Called as the very last thing before the uninstaller exits: cmd waits two seconds (by then this process is
/// gone), then deletes the folders uninstall() couldn't.
pub fn remove_after_exit() {
    let later: Vec<PathBuf> = PENDING.lock().map(|mut p| std::mem::take(&mut *p)).unwrap_or_default();
    if later.is_empty() {
        return;
    }
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let rm: Vec<String> = later.iter().map(|d| format!("rmdir /s /q \"{}\"", d.display())).collect();
    let script = format!("ping -n 3 127.0.0.1 >nul & {}", rm.join(" & "));
    // Verbatim: cmd.exe doesn't understand the backslash-escaped quotes Rust would add. /s strips the outer quotes.
    let _ = std::process::Command::new(Path::new(&root).join(r"System32\cmd.exe"))
        .raw_arg(format!("/d /s /c \"{script}\""))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nfsw-installer-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn sandbox_install_then_uninstall_leaves_nothing() {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        }
        let root = temp("full");
        let l = Layout::sandbox(&root);
        let setup = root.join("setup-src.exe");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&setup, b"setup").unwrap();
        let o = Options { autostart: true, desktop: true, user: r"PC\me".into() };
        let mut steps = Vec::new();
        install(&l, &o, b"app", &setup, &mut |i| steps.push(i)).unwrap();
        assert_eq!(steps, vec![0, 1, 2, 3]);
        assert_eq!(std::fs::read(l.exe()).unwrap(), b"app");
        assert_eq!(std::fs::read(l.uninstaller()).unwrap(), b"setup");
        assert!(l.start_menu.is_file() && l.desktop.is_file());
        let sys = l.sandbox.clone().unwrap();
        let entry = std::fs::read_to_string(sys.join("uninstall-entry.txt")).unwrap();
        assert!(entry.contains("--uninstall") && entry.contains("NoModify"));
        assert!(std::fs::read_to_string(sys.join("task.xml")).unwrap().contains("--tray"));

        // Reinstall without the extras: the desktop shortcut and the task go.
        install(&l, &Options { autostart: false, desktop: false, user: "x".into() }, b"app2", &setup, &mut |_| {}).unwrap();
        assert!(!l.desktop.exists() && !sys.join("task.xml").exists());
        assert_eq!(std::fs::read(l.exe()).unwrap(), b"app2");

        std::fs::write(l.data.join("config.json"), "{}").unwrap();
        uninstall(&l, false, &mut |_| {}).unwrap();
        assert!(!l.dir.exists() && !l.start_menu.exists() && !sys.join("uninstall-entry.txt").exists());
        assert!(l.data.join("config.json").exists(), "settings kept unless asked");
        uninstall(&l, true, &mut |_| {}).unwrap_or(());
        let _ = std::fs::remove_dir_all(&root);
    }
}
