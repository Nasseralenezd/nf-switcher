# NF Switcher

Pick, per app, whether it uses **Wi-Fi** or **Windows' default route** (Ethernet, which Windows prefers whenever
it's plugged in). Example: Brave over Wi-Fi, everything else (games included) over Ethernet.

A tray app with a small flyout above the clock (concept C, picked 2026-10-18). Rust, no frameworks: Win32 +
Direct2D. Every crate it uses was already in the local cargo cache, so building downloads nothing.

## Install

Download `NF-Switcher-Setup-<version>.exe` from [Releases](../../releases/latest) and run it.

- It installs for everyone on the PC into `C:\Program Files\NF Switcher` (or a folder you pick, which it makes
  admin-only too), adds a Start menu entry (and a desktop shortcut if you want), and an entry in
  Settings › Apps › Installed apps.
- **Start with Windows** is on by default: a sign-in task starts NF Switcher in the tray with admin rights, so
  there's no UAC prompt at every sign-in. Switch it off in the installer or later in NF Switcher's Settings.
- Windows asks for admin once (UAC) when you click Install. Running a newer setup updates in place and keeps your
  settings.
- **SmartScreen:** the setup isn't code-signed, so Windows may say it "protected your PC". Click "More info" →
  "Run anyway" if you trust the download. The release lists the file's SHA-256 so you can compare it
  (`Get-FileHash .\NF-Switcher-Setup-0.1.0.exe`).
- **Uninstall:** Settings › Apps › Installed apps › NF Switcher › Uninstall. It closes NF Switcher (which takes its
  rules out of Windows), removes the sign-in task, shortcuts, files and the Apps entry, and, if you tick it, your
  settings. `uninstall.exe --uninstall --quiet` does it without a window.

## Updates

About twice a day (20 seconds after start, then every 12 hours) NF Switcher asks GitHub for the latest release of
this repository: one HTTPS request to `api.github.com`, nothing about you is sent except what any web request
carries (your IP address). When a newer version is out, the Settings gear in the flyout gets a warning dot and
Settings shows "New update" with a Download button (it opens the release page in your browser, without admin).
Switch it off in Settings › Updates.

## Using it

- Start `nf-switcher.exe` (or "NF Switcher" in Start). **Windows asks for admin (UAC)**: routing needs it (see below). Say No and it still runs,
  read-only, with a "Restart as admin" button.
- Click the tray icon: the flyout shows Ethernet and Wi-Fi (connected or not, IP address), your apps with a Wi-Fi
  switch each, **Add an app** (running programs, or "Pick an .exe that isn't running…"), **Revert all**, and Settings.
- Switch an app on: its **new** connections go over Wi-Fi, including those of a copy that's already running. Each
  row says how many of its connections are on Wi-Fi and how many are still on the default route from before.
  Restart the app to move those, or right-click it and pick "Move its open connections now" (closes its IPv4 TCP
  connections not on Wi-Fi; the app reconnects over Wi-Fi).
- An app that isn't running yet is covered as soon as it starts.
- **Settings**:
  - what pinned apps do while Wi-Fi is down: use the default route, or block until Wi-Fi is back;
  - which adapter carries pinned apps (Automatic = the connected Wi-Fi; a phone's USB tethering works too);
  - "Keep the Ethernet network on Ethernet" (on by default): pinned apps still reach your router's page, a NAS or a
    printer on the Ethernet side's network. Only matters when Wi-Fi and Ethernet are different networks.
  - "Reconnect Wi-Fi when Windows drops it" (on by default): see Limits. The "Wi-Fi isn't connected" message also
    has a "Connect Wi-Fi" button.
  - "Start with Windows" and "Check for updates" (both on by default).
- **Revert all** puts every app back on the default route and removes everything NF Switcher added to Windows.
  The list stays, so switching apps back on is one click. Right-click an app to remove it from the list.
- **Quit** (tray menu) does the same as Revert all for Windows, but keeps your choices for next time.

Choices are saved in `%ProgramData%\NF Switcher\config.json` and applied again at the next start. That folder is
writable by admins only (NF Switcher sets this up): the app runs as admin and turns the file into firewall-level
rules, so ordinary programs must not be able to edit it. Windows' own programs and Microsoft Defender can never be
pinned, whatever the file says.

## What needs admin

| Needs admin (UAC) | Works without admin |
| --- | --- |
| Adding and removing the per-app connection policies and filters (Windows Filtering Platform) | Listing adapters, their status and addresses |
| Switching Windows' "Route Policies" TCP/IP option on for this boot, and back off | Listing processes, their paths and connections |
| Saving the settings (admin-only folder) | The flyout and the tray icon (read-only) |
| "Move its open connections now" (closes TCP connections) | |

NF Switcher starts as a normal app and asks for admin itself (the `runas` verb), so declining the prompt leaves a
working read-only app with a "Restart as admin" button. `--no-elevate` never asks.

## How the routing works

**Windows Filtering Platform connection policies** (`FwpmConnectionPolicyAdd0`, Windows 11 24H2 and later), added
from user mode in a *dynamic* WFP session. The full note with the trade-offs is at the top of `src/engine/wfp.rs`.
In short:

- One policy per pinned program says: its new connections leave through the Wi-Fi interface. A second, stronger
  policy keeps connections to the Ethernet side's own network on Ethernet.
- No driver, no download, no code injected into other apps, no proxy, no route or metric changes.
- Programs started later are covered, because a policy matches the .exe path, not a running process.
- Other apps' packets are untouched: Windows checks a policy once when a connection is set up, never per packet.
- **Wi-Fi down:** in the default mode the policies are removed, so pinned apps use the default route until Wi-Fi is
  back. In "Block until Wi-Fi is back" mode, filters allow pinned apps only Wi-Fi, loopback and the Ethernet LAN,
  so they fail at once instead of using Ethernet. These stay the same while Wi-Fi flaps, so nothing in Windows
  changes on every drop. Adapter changes come from Windows' own notifications; nothing polls.
- IPv6 of a pinned app to the internet is blocked while Wi-Fi has no IPv6, so the app falls back to IPv4 instead of
  leaking out over Ethernet's IPv6.
- **Windows' "Route Policies" option must be on**, or Windows accepts the policies and ignores them (measured on
  the development PC, build 26200, 2026-10-18). NF Switcher turns it on only while an app is pinned, for this boot
  only (`store=active`), after writing down what it found (`routepolicies.json`, per IPv4/IPv6). It turns back off
  only what was off, on exit or at the next start after a crash.
- **Cleanup:** Revert all, Quit, log-off and shutdown remove everything. If NF Switcher crashes or is killed,
  Windows removes the dynamic session's policies and filters itself (measured with `--selftest`).

### Limits

- **Only new connections move.** Connections opened before you switch an app keep their route until the app
  reconnects. Restarting the app moves everything; "Move its open connections now" does it for IPv4 TCP.
- **DNS:** lookups that go through Windows are sent by the DNS Client service, not by the app, so they follow the
  default route. That's usually harmless. Apps with their own DNS-over-HTTPS (Brave's "Secure DNS") follow the pin.
- **Windows 11 24H2 or later.** Older Windows doesn't have the API. NF Switcher says so and does nothing.
- **Wi-Fi must stay connected, and Windows drops it.** With Ethernet plugged in, Windows' default "minimize
  simultaneous connections" policy disconnects an automatically connected Wi-Fi: on the development PC this
  happened about 30 minutes after Ethernet came up (WLAN event log, 2026-10-18). NF Switcher never changes that
  policy. Instead, while apps are pinned, it connects Wi-Fi again with the profile Windows already has (at most
  once a minute; the same as clicking the network in Windows' Wi-Fi list), and the warning offers "Connect Wi-Fi".
  Not tested yet: the reconnect itself, and whether Windows keeps such a connection longer than an automatic one.
- Programs started by a pinned app keep the default route unless they're the same .exe (Brave's helper processes
  are `brave.exe` too). Add other .exe files separately. A pin applies to that .exe for every user of the PC.
- An app that updates itself into a new version folder (Discord) is covered within about 5 seconds; connections it
  opens in that moment stay on the default route until it reconnects.
- A pinned app goes around a full-tunnel VPN (over Wi-Fi). NF Switcher points this out when it sees one.
- Only one NF Switcher with admin can run on the PC at a time (the rules are machine-wide).

## Memory and CPU

Measured on the development PC, release build, 2026-10-18 (Windows' process counters):

- Running, flyout never opened (`--smoke-idle`: the real hidden window, without the tray icon): 14.2 MB working
  set, 2.3 MB private, 6 threads, 16 ms CPU in its first 5 s, start-up included.
- Flyout open (`--uitest`): 13 MB private when it opens, about 18 MB after going through every page (icons and
  fonts cached). Software Direct2D: a hardware target loaded the GPU driver and cost 62 MB. After it hides, the
  working set is trimmed to about 1.5 MB.
- Per packet: nothing to measure, because Windows applies a policy once, when a connection is set up (Microsoft's
  documentation of ALE layers; not measured here).

## Build and test

```bash
cargo build --release --offline
```

The installer: `powershell -NoProfile -File tools/build-setup.ps1` builds the app, then `nf-switcher-setup` with the
app built into it, and copies it to `dist\NF-Switcher-Setup-<version>.exe`. `--sandbox DIR` makes the setup install
and uninstall everything under DIR instead (no registry, no task, no Program Files), for testing.

- `cargo test --offline`: unit tests. They read this PC (adapters, processes, `netsh … show global`) but never
  change it; the engine is tested against a stand-in for WFP.
- `nf-switcher.exe --smoke`: hidden check without a window, tray icon or admin, data in `%TEMP%`. Prints one
  `SMOKE {...}` line and renders the real flyout to PNG. It refuses to run as admin.
- `nf-switcher.exe --shots DIR`: the flyout's sample states as PNG (dark and light), offscreen.
- `nf-switcher.exe --uitest`: shows the real flyout for about 12 s, clicks through it with real window messages,
  captures each page and measures memory (no admin).
- `nf-switcher.exe --selftest` (admin, one UAC prompt): pins throwaway copies of `probe.exe` and checks, with real
  TCP and UDP traffic, that a pinned copy moves to Wi-Fi (also one already running), that the Ethernet LAN stays
  on Ethernet, that blocking works and survives Windows re-checking open connections, that everything is removed,
  and that Windows removes the policy of a killed process. Report in
  `%ProgramData%\NF Switcher\selftest\report.txt`.
- `tools/make-icon.ps1` rebuilds `res/app.ico`. The Windows SDK's `rc.exe` embeds the icon, manifest and version
  info if it's installed; without it the app still builds.
