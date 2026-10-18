// NF Switcher's settings, journal and log live in %ProgramData%\NF Switcher, a folder only admins can change. The
// app runs elevated and turns what's in config.json into WFP rules, so the file must not be writable by every
// program the user runs (they could make it block or reroute other software), and nothing an unelevated program
// planted there (a link to a system file) may be written through.
use std::os::windows::fs::MetadataExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use windows::core::HSTRING;
use windows::Win32::Foundation::{HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows::Win32::Security::{
    SetFileSecurityW, DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
};
use windows::Win32::Storage::FileSystem::{CreateDirectoryW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION};

/// Owner Administrators; protected ACL: SYSTEM and Administrators full control, Users read (so the unelevated,
/// read-only NF Switcher can still show the settings).
const SDDL: &str = "O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;BU)";
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

pub fn machine_dir() -> PathBuf {
    let base = std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into());
    Path::new(&base).join("NF Switcher")
}

fn is_reparse(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
}

/// A file with more than one name could be a hard link to someone else's file.
fn extra_links(p: &Path) -> bool {
    let Ok(f) = std::fs::File::open(p) else { return false };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(HANDLE(f.as_raw_handle()), &mut info).is_ok() && info.nNumberOfLinks > 1 }
}

/// Elevated only. Creates the folder, or takes it over if it already exists (someone may have made it first):
/// owner Administrators and the protected ACL above. Then removes anything that was planted before the ACL was
/// ours: links, junctions and hard-linked files.
pub fn prepare(dir: &Path) -> Result<(), String> {
    if is_reparse(dir) {
        // A junction or link standing in for the folder: remove the link itself (not what it points to).
        std::fs::remove_dir(dir).or_else(|_| std::fs::remove_file(dir)).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    unsafe {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(&HSTRING::from(SDDL), SDDL_REVISION_1, &mut sd, None)
            .map_err(|e| e.message())?;
        let path = HSTRING::from(dir.as_os_str());
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: false.into(),
        };
        let _ = CreateDirectoryW(&path, Some(&sa));
        let ok = SetFileSecurityW(
            &path,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            sd,
        )
        .as_bool();
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        if !ok {
            return Err(format!("securing {}: {}", dir.display(), windows::core::Error::from_thread().message()));
        }
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if is_reparse(&p) {
                let _ = std::fs::remove_dir(&p).or_else(|_| std::fs::remove_file(&p));
            } else if p.is_file() && extra_links(&p) {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_dir_is_under_programdata() {
        assert!(machine_dir().ends_with("NF Switcher"));
    }

    #[test]
    fn hard_links_and_reparse_points_are_recognised() {
        // In a temp folder of the test's own; prepare() itself isn't run here (it needs admin to set the owner).
        let d = std::env::temp_dir().join(format!("nfsw-secure-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let a = d.join("a.txt");
        std::fs::write(&a, "x").unwrap();
        assert!(!extra_links(&a) && !is_reparse(&a));
        std::fs::hard_link(&a, d.join("b.txt")).unwrap();
        assert!(extra_links(&a));
        let _ = std::fs::remove_dir_all(&d);
    }
}
