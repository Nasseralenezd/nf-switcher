// Update check: asks GitHub for the latest NF Switcher release (one HTTPS GET to api.github.com, Windows' own WinHTTP,
// no extra crates) about twice a day, and the flyout shows a warning dot on Settings when it's newer. "Download"
// opens the release page in the user's browser *without* admin: NF Switcher runs elevated, so it asks Explorer's
// desktop to open the link (Explorer runs as the normal user), instead of starting an elevated browser itself.
use std::sync::Mutex;
use windows::core::{Interface, BSTR, PCWSTR};
use windows::Win32::Networking::WinHttp::*;
use windows::Win32::System::Com::{CoCreateInstance, IDispatch, IServiceProvider, CLSCTX_LOCAL_SERVER};
use windows::Win32::System::Variant::{VARIANT, VT_I4};
use windows::Win32::UI::Shell::*;

/// The GitHub repository releases are published to (owner/name).
pub const REPO: &str = "Nasseralenezd/nf-switcher";

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: (u32, u32, u32),
    pub tag: String,
    pub url: String,
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
    Some(Release { version: parse_version(&tag)?, tag, url })
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// GET https://api.github.com/<path>; returns (status, body).
fn get(path: &str) -> Result<(u32, String), String> {
    unsafe {
        let agent = wide(&format!("NF-Switcher/{}", env!("CARGO_PKG_VERSION")));
        let session = WinHttpOpen(PCWSTR(agent.as_ptr()), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0);
        if session.is_null() {
            return Err("WinHttpOpen failed".into());
        }
        let _ = WinHttpSetTimeouts(session, 5000, 5000, 5000, 10000);
        let host = wide("api.github.com");
        let conn = WinHttpConnect(session, PCWSTR(host.as_ptr()), 443, 0);
        let result = (|| {
            if conn.is_null() {
                return Err("can't reach api.github.com".to_string());
            }
            let verb = wide("GET");
            let obj = wide(path);
            let req = WinHttpOpenRequest(conn, PCWSTR(verb.as_ptr()), PCWSTR(obj.as_ptr()), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), WINHTTP_FLAG_SECURE);
            if req.is_null() {
                return Err("WinHttpOpenRequest failed".to_string());
            }
            let r = (|| {
                let headers: Vec<u16> = "Accept: application/vnd.github+json\r\n".encode_utf16().collect();
                WinHttpAddRequestHeaders(req, &headers, WINHTTP_ADDREQ_FLAG_ADD).map_err(|e| e.message())?;
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
                let mut buf = vec![0u8; 16 * 1024];
                loop {
                    let mut n = 0u32;
                    WinHttpReadData(req, buf.as_mut_ptr() as *mut _, buf.len() as u32, &mut n).map_err(|e| e.message())?;
                    if n == 0 || body.len() > 4 * 1024 * 1024 {
                        break;
                    }
                    body.extend_from_slice(&buf[..n as usize]);
                }
                Ok((status, String::from_utf8_lossy(&body).into_owned()))
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

fn variant_i4(v: i32) -> VARIANT {
    let mut var = VARIANT::default();
    unsafe {
        (*var.Anonymous.Anonymous).vt = VT_I4;
        (*var.Anonymous.Anonymous).Anonymous.lVal = v;
    }
    var
}

/// Opens a URL (or file) as the signed-in user, through Explorer's desktop (Raymond Chen's documented approach for
/// starting something unelevated from an elevated process). Only https:// links of this repository are opened.
pub fn open_unelevated(url: &str) -> Result<(), String> {
    if !url.starts_with(&format!("https://github.com/{REPO}/")) {
        return Err("not a link to NF Switcher's releases".into());
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
    fn only_our_links_are_opened() {
        assert!(open_unelevated("https://example.com/").is_err());
        assert!(open_unelevated("file:///C:/Windows/System32/cmd.exe").is_err());
    }
}
