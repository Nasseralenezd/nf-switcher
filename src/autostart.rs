// "Start with Windows": a Task Scheduler task that starts NF Switcher in the tray when the user signs in, with admin
// rights (highest privileges) so there's no UAC prompt at every sign-in. A Run-key entry can't do that: Windows
// silently skips apps that need admin there. The task has no time limit (the default would stop the app after
// three days) and runs at normal priority (the default is below normal). Creating or deleting it needs admin.
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const TASK: &str = "NF Switcher";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn schtasks() -> PathBuf {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    Path::new(&root).join(r"System32\schtasks.exe")
}

fn run(args: &[&str]) -> Result<String, String> {
    let out = Command::new(schtasks()).args(args).creation_flags(CREATE_NO_WINDOW).output().map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    if out.status.success() { Ok(text) } else { Err(text.trim().to_string()) }
}

/// DOMAIN\user of whoever is signed in (the task runs in their session).
pub fn current_user() -> String {
    let user = std::env::var("USERNAME").unwrap_or_default();
    match std::env::var("USERDOMAIN") {
        Ok(d) if !d.is_empty() => format!("{d}\\{user}"),
        _ => user,
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

/// The task definition: at `user`'s sign-in, start `exe --tray` with highest privileges in their session.
pub fn xml(exe: &Path, user: &str) -> String {
    let exe = esc(&exe.to_string_lossy());
    let user = esc(user);
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Author>NF Switcher</Author>
    <Description>Starts NF Switcher in the tray at sign-in, with admin rights and without a UAC prompt. Remove it in NF Switcher's Settings or by uninstalling NF Switcher.</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
      <Delay>PT5S</Delay>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>5</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>"{exe}"</Command>
      <Arguments>--tray</Arguments>
    </Exec>
  </Actions>
</Task>
"#
    )
}

/// schtasks wants the XML file in UTF-16 with a byte-order mark.
pub fn utf16_file(text: &str) -> Vec<u8> {
    let mut b = vec![0xFF, 0xFE];
    for u in text.encode_utf16() {
        b.extend_from_slice(&u.to_le_bytes());
    }
    b
}

/// Creates (or replaces) the task. `scratch` is a folder only admins can write (the XML goes there briefly).
pub fn create(exe: &Path, user: &str, scratch: &Path) -> Result<(), String> {
    let file = scratch.join("autostart-task.xml");
    std::fs::write(&file, utf16_file(&xml(exe, user))).map_err(|e| format!("writing the task: {e}"))?;
    let r = run(&["/Create", "/TN", TASK, "/XML", &file.to_string_lossy(), "/F"]);
    let _ = std::fs::remove_file(&file);
    r.map(|_| ())
}

pub fn delete() -> Result<(), String> {
    if !exists() {
        return Ok(());
    }
    run(&["/Delete", "/TN", TASK, "/F"]).map(|_| ())
}

pub fn exists() -> bool {
    run(&["/Query", "/TN", TASK]).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_xml_is_escaped_and_unlimited() {
        let x = xml(Path::new(r"C:\Program Files\NF Switcher\nf-switcher.exe"), r"PC\R&D <user>");
        assert!(x.contains(r"PC\R&amp;D &lt;user&gt;"));
        assert!(x.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
        assert!(x.contains("<RunLevel>HighestAvailable</RunLevel>"));
        assert!(x.contains(r#"<Command>"C:\Program Files\NF Switcher\nf-switcher.exe"</Command>"#));
        let f = utf16_file("ab");
        assert_eq!(f, vec![0xFF, 0xFE, b'a', 0, b'b', 0]);
    }

    #[test]
    fn query_is_read_only() {
        // Only reads the task list.
        let _ = exists();
    }
}
