// NF Switcher's settings, journal and log live in %ProgramData%\NF Switcher, a folder only admins can change. The
// app runs elevated and turns what's in config.json into WFP rules, so the file must not be writable by every
// program the user runs (they could make it block or reroute other software), and nothing an unelevated program
// planted there (a link to a system file) may be written through.
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use windows::core::{HSTRING, PWSTR};
use windows::Win32::Foundation::{HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    GetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT,
};
use windows::Win32::Security::{
    SetFileSecurityW, SetKernelObjectSecurity, DACL_SECURITY_INFORMATION, OBJECT_SECURITY_INFORMATION,
    OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
};
use windows::Win32::Storage::FileSystem::{CreateDirectoryW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION};

/// Owner Administrators; protected ACL: SYSTEM and Administrators full control, Users read (so the unelevated,
/// read-only NF Switcher can still show the settings).
const SDDL: &str = "O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;BU)";
/// Who can have made what's found in a folder that wasn't admin-only yet: an elevated program, or SYSTEM.
const ADMIN_OWNERS: [&str; 2] = ["O:BA", "O:SY"];
const SET: OBJECT_SECURITY_INFORMATION = OBJECT_SECURITY_INFORMATION(
    OWNER_SECURITY_INFORMATION.0 | DACL_SECURITY_INFORMATION.0 | PROTECTED_DACL_SECURITY_INFORMATION.0,
);
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const READ_CONTROL: u32 = 0x2_0000;
const WRITE_DAC: u32 = 0x4_0000;
const WRITE_OWNER: u32 = 0x8_0000;
const FILE_READ_ATTRIBUTES: u32 = 0x80;
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

static FAILED: AtomicBool = AtomicBool::new(false);

pub fn machine_dir() -> PathBuf {
    let base = std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into());
    Path::new(&base).join("NF Switcher")
}

/// prepare() failed in this run: nothing more is written into that folder (a link planted there may still be in
/// place, and writing through it would change someone else's file).
pub fn failed() -> bool {
    FAILED.load(Ordering::Relaxed)
}

/// A security descriptor made from SDDL, freed when dropped.
struct Sd(PSECURITY_DESCRIPTOR);

impl Sd {
    fn parse(sddl: &str) -> Result<Sd, String> {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(&HSTRING::from(sddl), SDDL_REVISION_1, &mut sd, None) }
            .map_err(|e| e.message())?;
        Ok(Sd(sd))
    }
}

impl Drop for Sd {
    fn drop(&mut self) {
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.0 .0)));
        }
    }
}

fn is_reparse(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
}

/// Opens the entry itself, never what a link or junction points to.
fn open(p: &Path, access: u32) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .access_mode(access)
        .share_mode(7)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(p)
}

/// An open entry's owner (and ACL) as SDDL: "O:BA", or "O:BAD:P(A;OICI;FA;;;SY)...".
fn sddl_of(f: &std::fs::File, what: OBJECT_SECURITY_INFORMATION) -> Option<String> {
    unsafe {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        GetSecurityInfo(HANDLE(f.as_raw_handle()), SE_FILE_OBJECT, what, None, None, None, None, Some(&mut sd)).ok().ok()?;
        let sd = Sd(sd);
        let mut s = PWSTR::null();
        ConvertSecurityDescriptorToStringSecurityDescriptorW(sd.0, SDDL_REVISION_1, what, &mut s, None).ok()?;
        let text = s.to_string().ok();
        let _ = LocalFree(Some(HLOCAL(s.0 as _)));
        text
    }
}

/// Elevated only. Creates the folder, or takes it over if it already exists (someone may have made it first):
/// owner Administrators and the protected ACL above, on the folder and on everything in it. What was planted
/// before the ACL was ours is removed: links, junctions, hard-linked files, and in a folder that wasn't admin-only
/// yet, anything an admin didn't make (its contents can't be trusted, and a program holding it open could keep
/// writing to it). Anything that can be neither secured nor removed is an error.
pub fn prepare(dir: &Path) -> Result<(), String> {
    let r = prepare_with(dir, SDDL, &ADMIN_OWNERS);
    if r.is_err() {
        FAILED.store(true, Ordering::Relaxed);
    }
    r
}

/// prepare() with the security and the trusted owners given (tests use their own: only an elevated program can
/// make Administrators the owner).
fn prepare_with(dir: &Path, sddl: &str, owners: &[&str]) -> Result<(), String> {
    if is_reparse(dir) {
        // A junction or link standing in for the folder: remove the link itself (not what it points to).
        std::fs::remove_dir(dir).or_else(|_| std::fs::remove_file(dir)).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let sd = Sd::parse(sddl)?;
    // Admin-only since an earlier start: only elevated programs can have put anything in it, so all of it stays
    // (even files owned by the admin's own account, which is how Windows can be set up to make them).
    let ours = open(dir, READ_CONTROL)
        .ok()
        .and_then(|f| sddl_of(&f, OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION))
        .is_some_and(|s| s == sddl);
    unsafe {
        let path = HSTRING::from(dir.as_os_str());
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0 .0,
            bInheritHandle: false.into(),
        };
        let _ = CreateDirectoryW(&path, Some(&sa));
        // The folder first: from here on nobody else can add anything to it.
        if !SetFileSecurityW(&path, SET, sd.0).as_bool() {
            return Err(format!("securing {}: {}", dir.display(), windows::core::Error::from_thread().message()));
        }
    }
    tidy(dir, &sd, (!ours).then_some(owners))
}

/// Secures everything in `dir`, all the way down, and removes what can't be trusted. `owners`: the folder wasn't
/// admin-only yet, so only what they made is kept.
fn tidy(dir: &Path, sd: &Sd, owners: Option<&[&str]>) -> Result<(), String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for e in rd {
        let p = e.map_err(|e| format!("{}: {e}", dir.display()))?.path();
        match secure_one(&p, sd, owners) {
            // A folder: secured before its contents, so nothing new can appear in it meanwhile.
            Some(true) => tidy(&p, sd, owners)?,
            Some(false) => {}
            None => remove(&p).map_err(|e| format!("{}: can't secure or remove it: {e}", p.display()))?,
        }
    }
    Ok(())
}

/// Secures one entry if it can be trusted, all through one handle (so it can't be swapped for a link meanwhile).
/// Some(is a folder) when it was kept, None when it has to go.
fn secure_one(p: &Path, sd: &Sd, owners: Option<&[&str]>) -> Option<bool> {
    let f = open(p, READ_CONTROL | WRITE_DAC | WRITE_OWNER | FILE_READ_ATTRIBUTES).ok()?;
    let h = HANDLE(f.as_raw_handle());
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(h, &mut info) }.ok()?;
    let folder = info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
    // A link or junction, or a file with a second name: a hard link could be someone else's file.
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 || (!folder && info.nNumberOfLinks > 1) {
        return None;
    }
    if let Some(owners) = owners {
        let owner = sddl_of(&f, OWNER_SECURITY_INFORMATION)?;
        if !owners.contains(&owner.as_str()) {
            return None;
        }
    }
    unsafe { SetKernelObjectSecurity(h, SET, sd.0) }.ok()?;
    Some(folder)
}

/// Removes an entry: a link itself (not what it points to), a file, or a folder with everything in it.
fn remove(p: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(p),
        _ => std::fs::remove_dir(p).or_else(|_| std::fs::remove_file(p)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::process::CommandExt;

    #[test]
    fn machine_dir_is_under_programdata() {
        assert!(machine_dir().ends_with("NF Switcher"));
    }

    // prepare() itself isn't run here: it needs admin to make Administrators the owner, and it would change the real
    // %ProgramData% folder. These run prepare_with() on temp folders of their own, with the test user as the owner.

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nfsw-secure-{name}-{}", std::process::id()));
        unlock_and_remove(&d);
        d
    }

    /// The test user as an owner ("O:S-1-5-21-..."), and the real security with that owner instead of Administrators,
    /// plus the right to set the owner again, which the elevated app has through Administrators.
    fn me(test: &str) -> (String, String) {
        let d = temp(&format!("{test}-me"));
        std::fs::create_dir_all(&d).unwrap();
        let owner = sddl_of(&open(&d, READ_CONTROL).unwrap(), OWNER_SECURITY_INFORMATION).unwrap();
        unlock_and_remove(&d);
        assert!(owner.starts_with("O:") && owner != "O:BA", "{owner}");
        let sddl = format!("{}(A;OICI;WO;;;{})", SDDL.replacen("O:BA", &owner, 1), &owner[2..]);
        (owner, sddl)
    }

    fn sddl_at(p: &Path) -> String {
        sddl_of(&open(p, READ_CONTROL).unwrap(), OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION).unwrap()
    }

    /// What the reviewer tried: appending to the file without admin.
    fn writable(p: &Path) -> bool {
        std::fs::OpenOptions::new().append(true).open(p).is_ok()
    }

    fn set_acl(p: &Path, sddl: &str) {
        let sd = Sd::parse(sddl).unwrap();
        let what = if sddl.starts_with("O:") { SET } else { DACL_SECURITY_INFORMATION };
        assert!(unsafe { SetFileSecurityW(&HSTRING::from(p.as_os_str()), what, sd.0) }.as_bool(), "{}", p.display());
    }

    /// The owner may always change the ACL: open everything up again, then delete it.
    fn unlock_and_remove(d: &Path) {
        fn walk(p: &Path) {
            if let Ok(sd) = Sd::parse("D:(A;OICI;FA;;;WD)") {
                unsafe {
                    let _ = SetFileSecurityW(&HSTRING::from(p.as_os_str()), DACL_SECURITY_INFORMATION, sd.0);
                }
            }
            for e in std::fs::read_dir(p).into_iter().flatten().flatten() {
                if e.file_type().is_ok_and(|t| !t.is_symlink()) {
                    walk(&e.path());
                }
            }
        }
        if d.exists() {
            walk(d);
            let _ = std::fs::remove_dir_all(d);
        }
    }

    fn junction(link: &Path, target: &Path) {
        let out = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .creation_flags(0x0800_0000)
            .output()
            .unwrap();
        assert!(out.status.success() && is_reparse(link), "{}", String::from_utf8_lossy(&out.stdout));
    }

    #[test]
    fn a_folder_that_wasnt_admin_only_loses_what_an_admin_didnt_make() {
        let (_, sddl) = me("planted");
        let (d, outside) = (temp("planted"), temp("planted-outside"));
        std::fs::create_dir_all(d.join("selftest")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        // Planted by an ordinary program before NF Switcher first ran elevated.
        std::fs::write(d.join("config.json"), r#"{"settings":{"when_wifi_down":"block"}}"#).unwrap();
        std::fs::write(d.join("selftest").join("report.json"), "{}").unwrap();
        std::fs::write(outside.join("theirs.txt"), "someone else's file").unwrap();
        std::fs::hard_link(outside.join("theirs.txt"), d.join("nf-switcher.log")).unwrap();
        junction(&d.join("link"), &outside);

        // The test user stands for that ordinary program: only SYSTEM counts as an admin here.
        prepare_with(&d, &sddl, &["O:SY"]).unwrap();
        let left: Vec<_> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name()).collect();
        assert!(left.is_empty(), "{left:?}");
        assert_eq!(sddl_at(&d), sddl);
        // What the link and the junction pointed to is untouched, ACL included.
        assert_eq!(std::fs::read_to_string(outside.join("theirs.txt")).unwrap(), "someone else's file");
        assert!(writable(&outside.join("theirs.txt")));
        assert_ne!(sddl_at(&outside), sddl);
        unlock_and_remove(&d);
        unlock_and_remove(&outside);
    }

    #[test]
    fn what_an_admin_made_is_kept_and_secured_all_the_way_down() {
        let (owner, sddl) = me("kept");
        let d = temp("kept");
        std::fs::create_dir_all(d.join("selftest")).unwrap();
        std::fs::write(d.join("config.json"), "{}").unwrap();
        std::fs::write(d.join("selftest").join("report.json"), "{}").unwrap();
        assert!(writable(&d.join("config.json")));

        // The test user stands for the admin who made them.
        prepare_with(&d, &sddl, &[owner.as_str()]).unwrap();
        for p in [d.clone(), d.join("config.json"), d.join("selftest"), d.join("selftest").join("report.json")] {
            assert_eq!(sddl_at(&p), sddl, "{}", p.display());
        }
        assert_eq!(std::fs::read_to_string(d.join("config.json")).unwrap(), "{}");
        if !crate::elevate::is_elevated() {
            // Without admin: readable, but no longer writable, and nothing new can be added.
            assert!(!writable(&d.join("config.json")) && !writable(&d.join("selftest").join("report.json")));
            assert!(std::fs::write(d.join("new.json"), "{}").is_err());
        }
        unlock_and_remove(&d);
    }

    #[test]
    fn an_admin_only_folder_keeps_everything_in_it() {
        let (owner, sddl) = me("ours");
        let d = temp("ours");
        prepare_with(&d, &sddl, &["O:SY"]).unwrap();
        assert_eq!(sddl_at(&d), sddl);
        // An elevated NF Switcher saves its settings; with Windows set to make the creator the owner, that's the
        // admin's own account rather than Administrators.
        set_acl(&d, &format!("D:(A;OICI;FA;;;{})", &owner[2..]));
        std::fs::write(d.join("config.json"), "{}").unwrap();
        set_acl(&d, &sddl);

        prepare_with(&d, &sddl, &["O:SY"]).unwrap();
        assert_eq!(std::fs::read_to_string(d.join("config.json")).unwrap(), "{}");
        assert_eq!(sddl_at(&d.join("config.json")), sddl);
        unlock_and_remove(&d);
    }

    #[test]
    fn what_can_be_neither_secured_nor_removed_fails() {
        let (owner, sddl) = me("held");
        // Planted and held open by a program that doesn't let it be deleted.
        let d = temp("held");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("config.json"), "{}").unwrap();
        let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(d.join("config.json")).unwrap();
        let e = prepare_with(&d, &sddl, &["O:SY"]).unwrap_err();
        assert!(e.contains("config.json") && e.contains("can't secure or remove it"), "{e}");
        drop(held);
        unlock_and_remove(&d);

        // Made by the admin, but its ACL doesn't let its owner be set, and the folder doesn't let it be deleted.
        if !crate::elevate::is_elevated() {
            let d = temp("stuck");
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("config.json"), "{}").unwrap();
            set_acl(&d.join("config.json"), &format!("D:P(A;;FR;;;{})", &owner[2..]));
            let e = prepare_with(&d, &sddl, &[owner.as_str()]).unwrap_err();
            assert!(e.contains("config.json"), "{e}");
            unlock_and_remove(&d);
        }
    }
}
