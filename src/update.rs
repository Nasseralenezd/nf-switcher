// Update check: asks GitHub for the latest NF Switcher release (one HTTPS GET to api.github.com, Windows' own WinHTTP,
// no extra crates) once when NF Switcher starts, and the flyout shows a warning dot on Settings when it's newer.
// "Update now" (with admin) downloads the release's setup into the admin-only settings folder, checks it against the
// SHA-256 GitHub lists for it, and runs it in update mode: it replaces NF Switcher and starts the new one.
// "Download" (without admin, or when that fails) opens the release page in the user's browser *without* admin:
// NF Switcher runs elevated, so it asks Explorer's desktop to open the link (Explorer runs as the normal user),
// instead of starting an elevated browser itself.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use windows::core::{Interface, BSTR, PCWSTR};
use windows::Win32::Networking::WinHttp::*;
use windows::Win32::Security::Cryptography::{BCryptHash, BCRYPT_SHA256_ALG_HANDLE};
use windows::Win32::System::Com::{CoCreateInstance, IDispatch, IServiceProvider, CLSCTX_LOCAL_SERVER};
use windows::Win32::System::Variant::{VARIANT, VT_I4};
use windows::Win32::UI::Shell::*;

/// The GitHub repository releases are published to (owner/name).
pub const REPO: &str = "Nasseralenezd/nf-switcher";

/// No setup is anywhere near this big (about 1.5 MB); a bigger "setup" isn't downloaded.
const MAX_SETUP: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: (u32, u32, u32),
    pub tag: String,
    pub url: String,
    /// The release's setup, when it can be installed from inside NF Switcher.
    pub setup: Option<Setup>,
}

/// A release's setup file, as GitHub lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct Setup {
    pub url: String,
    pub size: u64,
    /// Lowercase hex.
    pub sha256: String,
}

impl Release {
    pub fn version_text(&self) -> String {
        format!("{}.{}.{}", self.version.0, self.version.1, self.version.2)
    }
}

/// "v1.2.3" or "1.2" -> (1, 2, 3) / (1, 2, 0). Anything after a '-' (pre-release) is ignored.
pub fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let s = s.trim().trim_start_matches(['v', 'V']);
    let s = s.split(['-', '+']).next()?;
    let mut it = s.split('.').map(|p| p.parse::<u32>());
    let major = it.next()?.ok()?;
    let minor = it.next().unwrap_or(Ok(0)).ok()?;
    let patch = it.next().unwrap_or(Ok(0)).ok()?;
    Some((major, minor, patch))
}

pub fn current() -> (u32, u32, u32) {
    parse_version(env!("CARGO_PKG_VERSION")).unwrap_or((0, 0, 0))
}

/// The release from GitHub's JSON, if it's a real (not draft, not pre-release) one.
pub fn parse_release(json: &str) -> Option<Release> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    if v["draft"].as_bool() == Some(true) || v["prerelease"].as_bool() == Some(true) {
        return None;
    }
    let tag = v["tag_name"].as_str()?.to_string();
    let url = v["html_url"].as_str().unwrap_or("").to_string();
    // Only ever open a page of this repository.
    let ours = format!("https://github.com/{REPO}/");
    let url = if url.starts_with(&ours) { url } else { format!("https://github.com/{REPO}/releases/latest") };
    // Only this repository's setup, with a SHA-256 to check it against; anything else goes through the browser.
    let setup = v["assets"].as_array().into_iter().flatten().find_map(|a| {
        let name = a["name"].as_str()?;
        let link = a["browser_download_url"].as_str()?;
        let size = a["size"].as_u64()?;
        let sha256 = a["digest"].as_str()?.strip_prefix("sha256:")?.to_ascii_lowercase();
        let ok = link.starts_with(&format!("https://github.com/{REPO}/releases/download/"))
            && !link.contains("..")
            && name.starts_with("NF-Switcher-Setup-")
            && name.ends_with(".exe")
            && (1..=MAX_SETUP).contains(&size)
            && sha256.len() == 64
            && sha256.bytes().all(|b| b.is_ascii_hexdigit());
        ok.then(|| Setup { url: link.into(), size, sha256 })
    });
    Some(Release { version: parse_version(&tag)?, tag, url, setup })
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// GET https://api.github.com/<path>; returns (status, body).
fn get(path: &str) -> Result<(u32, String), String> {
    let (status, body) = fetch("api.github.com", path, Some("application/vnd.github+json"), 4 * 1024 * 1024, &mut |_| {})?;
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

/// GET https://<host><path>, HTTPS only. WinHTTP follows redirects (a release download goes on to GitHub's file
/// host) but never from HTTPS to HTTP. More than `limit` bytes is an error; `progress` gets the bytes so far.
fn fetch(host: &str, path: &str, accept: Option<&str>, limit: usize, progress: &mut dyn FnMut(usize)) -> Result<(u32, Vec<u8>), String> {
    unsafe {
        let agent = wide(&format!("NF-Switcher/{}", env!("CARGO_PKG_VERSION")));
        let session = WinHttpOpen(PCWSTR(agent.as_ptr()), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0);
        if session.is_null() {
            return Err("WinHttpOpen failed".into());
        }
        let _ = WinHttpSetTimeouts(session, 5000, 5000, 5000, 10000);
        let whost = wide(host);
        let conn = WinHttpConnect(session, PCWSTR(whost.as_ptr()), 443, 0);
        let result = (|| {
            if conn.is_null() {
                return Err(format!("can't reach {host}"));
            }
            let verb = wide("GET");
            let obj = wide(path);
            let req = WinHttpOpenRequest(conn, PCWSTR(verb.as_ptr()), PCWSTR(obj.as_ptr()), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), WINHTTP_FLAG_SECURE);
            if req.is_null() {
                return Err("WinHttpOpenRequest failed".to_string());
            }
            let r = (|| {
                if let Some(accept) = accept {
                    let headers: Vec<u16> = format!("Accept: {accept}\r\n").encode_utf16().collect();
                    WinHttpAddRequestHeaders(req, &headers, WINHTTP_ADDREQ_FLAG_ADD).map_err(|e| e.message())?;
                }
                WinHttpSendRequest(req, None, None, 0, 0, 0).map_err(|e| format!("can't reach GitHub ({})", e.message()))?;
                WinHttpReceiveResponse(req, std::ptr::null_mut()).map_err(|e| format!("no answer from GitHub ({})", e.message()))?;
                let mut status = 0u32;
                let mut len = 4u32;
                let mut idx = 0u32;
                WinHttpQueryHeaders(
                    req,
                    WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                    PCWSTR::null(),
                    Some(&mut status as *mut u32 as *mut _),
                    &mut len,
                    &mut idx,
                )
                .map_err(|e| e.message())?;
                let mut body = Vec::new();
                let mut buf = vec![0u8; 64 * 1024];
                loop {
                    let mut n = 0u32;
                    WinHttpReadData(req, buf.as_mut_ptr() as *mut _, buf.len() as u32, &mut n)
                        .map_err(|e| format!("the download stopped ({})", e.message()))?;
                    if n == 0 {
                        break;
                    }
                    body.extend_from_slice(&buf[..n as usize]);
                    if body.len() > limit {
                        return Err("GitHub sent more than expected".into());
                    }
                    progress(body.len());
                }
                Ok((status, body))
            })();
            let _ = WinHttpCloseHandle(req);
            r
        })();
        if !conn.is_null() {
            let _ = WinHttpCloseHandle(conn);
        }
        let _ = WinHttpCloseHandle(session);
        result
    }
}

/// The newest published release on GitHub. Ok(None) when the repository has no release yet.
pub fn latest() -> Result<Option<Release>, String> {
    let (status, body) = get(&format!("/repos/{REPO}/releases/latest"))?;
    match status {
        200 => Ok(parse_release(&body)),
        404 => Ok(None),
        403 | 429 => Err("GitHub asked to slow down; trying again later".into()),
        s => Err(format!("GitHub answered {s}")),
    }
}

/// The finished background check, picked up by the window when it's told.
static RESULT: Mutex<Option<Result<Option<Release>, String>>> = Mutex::new(None);

/// Checks on a background thread (the network can take seconds), then calls `done` (which posts to the window).
pub fn check_async(done: impl FnOnce() + Send + 'static) {
    std::thread::spawn(move || {
        let r = latest();
        if let Ok(mut slot) = RESULT.lock() {
            *slot = Some(r);
        }
        done();
    });
}

pub fn take_result() -> Option<Result<Option<Release>, String>> {
    RESULT.lock().ok().and_then(|mut s| s.take())
}

/// Where "Update now" keeps the downloaded setup: inside NF Switcher's settings folder, which only admins can write
/// to, so nothing can swap the file between the check and the start.
pub fn download_dir() -> PathBuf {
    crate::config::data_dir().join("update")
}

/// Setups an earlier update downloaded (removed at start, when that setup has long finished).
pub fn clean_downloads() {
    let _ = std::fs::remove_dir_all(download_dir());
}

pub fn sha256_hex(data: &[u8]) -> Result<String, String> {
    let mut out = [0u8; 32];
    unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, data, &mut out) }
        .ok()
        .map_err(|e| format!("SHA-256 failed ({})", e.message()))?;
    Ok(out.iter().map(|b| format!("{b:02x}")).collect())
}

/// A downloaded setup that matched its SHA-256, held open so nothing can change it until it's started.
pub struct Checked {
    pub path: PathBuf,
    _lock: std::fs::File,
}

/// Downloads the release's setup into `dir`. It's written only if its size and SHA-256 match what GitHub lists,
/// then opened so no one else can write to it or replace it, and checked once more through that handle.
/// `progress` gets 0-100.
pub fn download(rel: &Release, dir: &Path, progress: &mut dyn FnMut(u32)) -> Result<Checked, String> {
    if crate::secure::failed() {
        return Err("NF Switcher's settings folder couldn't be secured".into());
    }
    let s = rel.setup.as_ref().ok_or("this release has no setup to download")?;
    let path = s.url.strip_prefix("https://github.com").ok_or("not a GitHub link")?;
    let size = s.size as usize;
    let (status, body) = fetch("github.com", path, None, size, &mut |n| progress((n * 100 / size.max(1)).min(100) as u32))?;
    if status != 200 {
        return Err(format!("GitHub answered {status}"));
    }
    if body.len() != size {
        return Err(format!("the download stopped at {} of {size} bytes", body.len()));
    }
    if sha256_hex(&body)? != s.sha256 {
        return Err("the download doesn't match the SHA-256 GitHub lists for it".into());
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    save_locked(&dir.join(format!("NF-Switcher-Setup-{}.exe", rel.version_text())), &body)
}

/// Writes `body` to `file`, then holds it open for reading only (others may read, never write, rename or delete)
/// and checks what's in it through that handle.
fn save_locked(file: &Path, body: &[u8]) -> Result<Checked, String> {
    use std::io::Read;
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 1;
    let _ = std::fs::remove_file(file);
    let saved = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .share_mode(0)
        .open(file)
        .and_then(|mut f| std::io::Write::write_all(&mut f, body));
    if let Err(e) = saved {
        let _ = std::fs::remove_file(file);
        return Err(format!("saving the setup: {e}"));
    }
    // Readers only from here on (starting it is a read); what's in it is checked again through this handle.
    let mut lock = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(file)
        .map_err(|e| format!("opening the setup: {e}"))?;
    let mut again = Vec::with_capacity(body.len());
    lock.read_to_end(&mut again).map_err(|e| format!("reading the setup back: {e}"))?;
    if again != body {
        return Err("the saved setup changed after its check".into());
    }
    Ok(Checked { path: file.to_path_buf(), _lock: lock })
}

/// The finished download, and how far it got (0-100) while it runs.
static DOWNLOAD: Mutex<Option<Result<Checked, String>>> = Mutex::new(None);
static PERCENT: AtomicU32 = AtomicU32::new(0);

/// Downloads on a background thread. `progress` is called when the percentage changes, `done` at the end (both
/// post to the window, which reads `download_percent` and `take_download`).
pub fn download_async(rel: Release, progress: impl Fn() + Send + 'static, done: impl FnOnce() + Send + 'static) {
    PERCENT.store(0, Ordering::Relaxed);
    std::thread::spawn(move || {
        let mut last = u32::MAX;
        let r = download(&rel, &download_dir(), &mut |p| {
            if p != last {
                last = p;
                PERCENT.store(p, Ordering::Relaxed);
                progress();
            }
        });
        if let Ok(mut slot) = DOWNLOAD.lock() {
            *slot = Some(r);
        }
        done();
    });
}

pub fn download_percent() -> u32 {
    PERCENT.load(Ordering::Relaxed)
}

pub fn take_download() -> Option<Result<Checked, String>> {
    DOWNLOAD.lock().ok().and_then(|mut s| s.take())
}

/// Starts the checked setup in update mode (it stays locked until it's running). NF Switcher runs as admin, so the
/// setup does too (no UAC prompt): it closes this copy (which takes its rules out of Windows), replaces it and
/// starts the new one.
pub fn run_setup(setup: Checked) -> Result<(), String> {
    let r = std::process::Command::new(&setup.path).arg("--update").spawn();
    drop(setup);
    r.map(|_| ()).map_err(|e| format!("starting the setup: {e}"))
}

fn variant_i4(v: i32) -> VARIANT {
    let mut var = VARIANT::default();
    unsafe {
        (*var.Anonymous.Anonymous).vt = VT_I4;
        (*var.Anonymous.Anonymous).Anonymous.lVal = v;
    }
    var
}

/// Windows Update's page in Settings (the "needs Windows 11 24H2" message opens it).
pub const WINDOWS_UPDATE: &str = "ms-settings:windowsupdate";

/// What NF Switcher may open: this repository's https:// pages and Windows Update's page, nothing else.
fn may_open(url: &str) -> bool {
    url.starts_with(&format!("https://github.com/{REPO}/")) || url == WINDOWS_UPDATE
}

/// Opens a URL as the signed-in user, through Explorer's desktop (Raymond Chen's documented approach for starting
/// something unelevated from an elevated process). Only what `may_open` allows is opened.
pub fn open_unelevated(url: &str) -> Result<(), String> {
    if !may_open(url) {
        return Err("not a link NF Switcher opens".into());
    }
    unsafe {
        let windows: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER).map_err(|e| e.message())?;
        let loc = variant_i4(CSIDL_DESKTOP as i32);
        let empty = VARIANT::default();
        let mut hwnd = 0i32;
        let disp: IDispatch = windows
            .FindWindowSW(&loc, &empty, SWC_DESKTOP, &mut hwnd, SWFO_NEEDDISPATCH)
            .map_err(|e| format!("no desktop window ({})", e.message()))?;
        let sp: IServiceProvider = disp.cast().map_err(|e| e.message())?;
        let browser: IShellBrowser = sp.QueryService(&SID_STopLevelBrowser).map_err(|e| e.message())?;
        let view = browser.QueryActiveShellView().map_err(|e| e.message())?;
        let bg: IDispatch = view.GetItemObject(SVGIO_BACKGROUND).map_err(|e| e.message())?;
        let folder_view: IShellFolderViewDual = bg.cast().map_err(|e| e.message())?;
        let shell: IShellDispatch2 = folder_view.Application().map_err(|e| e.message())?.cast().map_err(|e| e.message())?;
        let show = variant_i4(1); // SW_SHOWNORMAL
        let none = VARIANT::default();
        shell.ShellExecute(&BSTR::from(url), &none, &none, &none, &show).map_err(|e| e.message())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v0.2.0"), Some((0, 2, 0)));
        assert_eq!(parse_version("1.10"), Some((1, 10, 0)));
        assert_eq!(parse_version("v2.0.1-beta.1"), Some((2, 0, 1)));
        assert_eq!(parse_version("latest"), None);
        assert!((0, 2, 0) > (0, 1, 9) && (1, 0, 0) > (0, 99, 99));
        assert_eq!(current(), parse_version(env!("CARGO_PKG_VERSION")).unwrap());
    }

    #[test]
    fn releases_from_github_json() {
        let j = format!(r#"{{"tag_name":"v0.2.0","html_url":"https://github.com/{REPO}/releases/tag/v0.2.0","draft":false,"prerelease":false}}"#);
        let r = parse_release(&j).unwrap();
        assert_eq!(r.version, (0, 2, 0));
        assert_eq!(r.version_text(), "0.2.0");
        assert!(r.url.ends_with("/releases/tag/v0.2.0"));
        // Drafts and pre-releases don't count; a foreign link is replaced by our releases page.
        assert!(parse_release(r#"{"tag_name":"v9.0.0","draft":true}"#).is_none());
        assert!(parse_release(r#"{"tag_name":"v9.0.0","prerelease":true}"#).is_none());
        let r = parse_release(r#"{"tag_name":"v9.0.0","html_url":"https://evil.example/x"}"#).unwrap();
        assert!(r.url.starts_with(&format!("https://github.com/{REPO}/")));
    }

    #[test]
    fn the_setup_comes_only_from_this_repository_with_a_sha256() {
        let sha = "0DFC7FA2D68BE2D38A65C563E3F9D1F10BF26E1DF15EA93A50E70DD3232A1942";
        let ours = format!("https://github.com/{REPO}/releases/download/v0.2.0/NF-Switcher-Setup-0.2.0.exe");
        let asset = |name: &str, link: &str, digest: &str| {
            format!(r#"{{"name":"{name}","browser_download_url":"{link}","size":1582080,"digest":"{digest}"}}"#)
        };
        let rel = |assets: Vec<String>| parse_release(&format!(r#"{{"tag_name":"v0.2.0","assets":[{}]}}"#, assets.join(","))).unwrap();
        let digest = format!("sha256:{sha}");
        let r = rel(vec![asset("notes.txt", &ours, &digest), asset("NF-Switcher-Setup-0.2.0.exe", &ours, &digest)]);
        let s = r.setup.unwrap();
        assert_eq!((s.url.as_str(), s.size, s.sha256.as_str()), (ours.as_str(), 1582080, sha.to_ascii_lowercase().as_str()));
        // Another repository, no SHA-256, or none at all: no in-app update (the browser instead).
        let theirs = "https://github.com/someone/else/releases/download/v0.2.0/NF-Switcher-Setup-0.2.0.exe";
        assert!(rel(vec![asset("NF-Switcher-Setup-0.2.0.exe", theirs, &digest)]).setup.is_none());
        assert!(rel(vec![asset("NF-Switcher-Setup-0.2.0.exe", &ours, "")]).setup.is_none());
        assert!(rel(vec![asset("NF-Switcher-Setup-0.2.0.exe", &ours, "sha1:a9993e364706816aba3e")]).setup.is_none());
        assert!(rel(vec![]).setup.is_none());
    }

    #[test]
    fn a_saved_setup_stays_locked_until_it_is_started() {
        let d = std::env::temp_dir().join(format!("nfsw-update-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("NF-Switcher-Setup-9.0.0.exe");
        let checked = save_locked(&f, b"setup bytes").unwrap();
        // Reading (which is what starting it does) works; writing, replacing or deleting it doesn't.
        assert_eq!(std::fs::read(&f).unwrap(), b"setup bytes");
        assert!(std::fs::OpenOptions::new().write(true).open(&f).is_err());
        assert!(std::fs::rename(&f, d.join("other.exe")).is_err());
        assert!(std::fs::remove_file(&f).is_err());
        drop(checked);
        assert!(std::fs::OpenOptions::new().write(true).open(&f).is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn sha256_of_a_known_text() {
        assert_eq!(sha256_hex(b"abc").unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn only_our_links_are_opened() {
        assert!(open_unelevated("https://example.com/").is_err());
        assert!(open_unelevated("file:///C:/Windows/System32/cmd.exe").is_err());
        // Checked without opening anything (no window during tests).
        assert!(may_open(&format!("https://github.com/{REPO}/releases/latest")));
        assert!(may_open(WINDOWS_UPDATE));
        assert!(!may_open("ms-settings:network"));
        assert!(!may_open("ms-settings:windowsupdate-action"));
        assert!(!may_open(&format!("http://github.com/{REPO}/")));
    }
}
