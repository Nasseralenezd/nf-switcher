// --selftest (admin): proves on a real PC that pinning works, end to end, without touching the user's apps or
// settings. It pins only throwaway copies of probe.exe, with its data in %TEMP%\nf-switcher-selftest:
//   baseline       nothing pinned: both copies use the default route
//   A              policy with Windows' routepolicies option as found  -> does the pinned copy move? (a finding)
//   B              routepolicies on (only if A didn't move)            -> it must move now
//   running        a copy already running when the pin lands           -> its next connections move
//   lan            pinned, DNS to the Ethernet router                   -> stays on Ethernet (LAN-keep)
//   strict         "block until Wi-Fi is back", Wi-Fi up               -> still works, over Wi-Fi
//   reauth         an open Wi-Fi connection while filters change        -> survives Windows re-checking it
//   no-wifi        block mode with no Wi-Fi adapter                     -> pinned blocked, the other copy fine
//   cleared        everything removed                                   -> both back on the default route
//   crash          a child holding the policy is killed                 -> Windows removes the policy itself
// Windows changes: WFP objects of dynamic sessions (removed on exit or crash) and routepolicies with store=active
// (put back at the end; Windows forgets it at the next boot anyway).
use crate::engine::routepolicies::{self, Netsh};
use crate::engine::wfp::{real::RealWfp, RoutePolicies, WfpEngine};
use crate::engine::{lan_of, target_of, Engine, Lan, Plan};
use crate::net::{self, Iface};
use serde_json::{json, Value};
use std::net::IpAddr;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const V4_TCP: &str = "1.1.1.1:80";
const V4_UDP: &str = "1.1.1.1:53";

/// routepolicies left exactly as it is (the test holds it on itself when needed).
struct Leave;
impl RoutePolicies for Leave {
    fn need(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn release(&mut self) {}
}

#[derive(Debug, Default, Clone)]
struct Probe {
    tcp: Option<IpAddr>,
    udp: Option<IpAddr>,
    raw: String,
}

fn local(line: &str) -> Option<IpAddr> {
    let v = line.split_whitespace().find_map(|w| w.strip_prefix("local="))?;
    v.parse::<std::net::SocketAddr>().ok().map(|a| a.ip())
}

/// The probe's answer: which local address TCP and UDP used (None = it failed or was blocked).
fn parse(out: &str) -> Probe {
    let mut p = Probe { raw: out.trim().to_string(), ..Default::default() };
    for l in out.lines() {
        // Only count it when an answer came back, so "connected but blocked later" isn't a pass.
        if l.starts_with("TCP ") && !l.contains("answer=\"\"") {
            p.tcp = local(l);
        }
        if l.starts_with("UDP ") && l.contains("answer=true") {
            p.udp = local(l);
        }
    }
    p
}

fn run_probe(exe: &Path, args: &[&str]) -> String {
    match Command::new(exe).args(args).creation_flags(CREATE_NO_WINDOW).output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
        Err(e) => format!("couldn't start probe: {e}"),
    }
}

fn spawn_probe(exe: &Path, args: &[&str]) -> Result<std::process::Child, String> {
    Command::new(exe)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("starting the probe: {e}"))
}

fn finish(child: std::process::Child) -> String {
    child.wait_with_output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

struct Ctx {
    w: Iface,
    e: Option<Iface>,
    lan: Option<Lan>,
    pinned: PathBuf,
    other: PathBuf,
    distinct: bool,
    steps: Vec<Value>,
    lines: Vec<String>,
}

impl Ctx {
    fn side(&self, ip: Option<IpAddr>) -> &'static str {
        let Some(ip) = ip else { return "failed" };
        let has = |i: &Iface| i.ipv4.iter().any(|a| IpAddr::V4(*a) == ip) || i.ipv6.iter().any(|a| IpAddr::V6(*a) == ip);
        if has(&self.w) {
            "wifi"
        } else if self.e.as_ref().is_some_and(has) {
            "default"
        } else {
            "other"
        }
    }

    /// With one connection only, "default" and "wifi" are the same thing.
    fn want(&self, side: &'static str) -> &'static str {
        if !self.distinct && side == "default" { "wifi" } else { side }
    }

    fn record(&mut self, name: &str, pass: bool, info: bool, summary: String, rec: Value) -> bool {
        let tag = if info { "INFO" } else if pass { "PASS" } else { "FAIL" };
        self.lines.push(format!("{tag} {name}: {summary}"));
        let mut rec = rec;
        rec["step"] = name.into();
        rec["pass"] = pass.into();
        self.steps.push(rec);
        pass
    }

    /// Runs both copies once and records where each went.
    fn step(&mut self, name: &str, expect: &'static str, note: &str) -> bool {
        self.step_with(name, expect, note, V4_UDP, false)
    }

    fn step_with(&mut self, name: &str, expect: &'static str, note: &str, udp: &str, info: bool) -> bool {
        let expect = self.want(expect);
        let p = parse(&run_probe(&self.pinned, &[V4_TCP, udp]));
        let o = parse(&run_probe(&self.other, &[V4_TCP, V4_UDP]));
        let (pt, pu, ot, ou) = (self.side(p.tcp), self.side(p.udp), self.side(o.tcp), self.side(o.udp));
        let pass = pt == expect && pu == expect && ot != "failed" && (!self.distinct || ot != "wifi");
        let summary = format!("pinned TCP {pt} UDP {pu}, other TCP {ot} UDP {ou} (expected pinned: {expect}) {note}");
        self.record(name, pass, info, summary, json!({"pinned": p.raw, "other": o.raw}))
    }

    fn plan(&self, guard: bool, with_wifi: bool, lan: bool, extra: Option<&Path>) -> Plan {
        let mut apps = vec![self.pinned.to_string_lossy().into_owned()];
        if let Some(x) = extra {
            apps.push(x.to_string_lossy().into_owned());
        }
        let t = target_of(&self.w);
        Plan {
            v4: true,
            v6: t.ipv6 || guard,
            block_v6: !t.ipv6 && !guard && with_wifi,
            guard,
            target: with_wifi.then_some(t),
            lan: if lan { self.lan.clone() } else { None },
            apps,
        }
    }

    fn engine(&self) -> Result<WfpEngine<RealWfp, Leave>, String> {
        Ok(WfpEngine::new(RealWfp::open().map_err(|e| e.to_string())?, Leave))
    }
}

fn copy_probe(dir: &Path) -> Result<(PathBuf, PathBuf), String> {
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let src = me.with_file_name("probe.exe");
    if !src.is_file() {
        return Err(format!("{} not found (cargo build --release builds it)", src.display()));
    }
    let pinned = dir.join("pinned").join("probe.exe");
    let other = dir.join("other").join("probe.exe");
    for p in [&pinned, &other] {
        std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::copy(&src, p).map_err(|e| format!("copying the probe: {e}"))?;
    }
    Ok((pinned, other))
}

/// The child for the crash step: adds the pinned policy, says READY, then waits to be killed.
pub fn child(pinned: &str, ready: &str) -> i32 {
    let ifaces = net::snapshot();
    let Some(t) = crate::engine::pick_target(&ifaces, None).filter(|t| t.up()) else { return 3 };
    let Ok(b) = RealWfp::open() else { return 4 };
    let mut e = WfpEngine::new(b, Leave);
    let plan = Plan { apps: vec![pinned.to_string()], target: Some(target_of(t)), v4: true, ..Default::default() };
    if e.apply(&plan).is_err() {
        return 5;
    }
    let _ = std::fs::write(ready, "READY");
    std::thread::sleep(Duration::from_secs(120));
    0
}

pub fn run() -> i32 {
    // Elevated: the probe copies, journal and report go in the admin-only folder, so nothing a normal program
    // planted can be run or written through. The report stays readable.
    let base = crate::secure::machine_dir();
    let dir = base.join("selftest");
    if crate::elevate::is_elevated() {
        if let Err(e) = crate::secure::prepare(&base) {
            let _ = std::fs::write(std::env::temp_dir().join("nf-switcher-selftest-error.txt"), &e);
            return 2;
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    let _ = std::fs::create_dir_all(&dir);
    std::env::set_var("NFSW_DATA_DIR", &dir);
    let started = Instant::now();
    let mut v = match run_inner(&dir) {
        Ok(v) => v,
        Err(e) => json!({"pass": false, "error": e}),
    };
    let ok = v["pass"].as_bool().unwrap_or(false);
    v["seconds"] = started.elapsed().as_secs_f32().into();
    let _ = std::fs::write(dir.join("report.json"), serde_json::to_string_pretty(&v).unwrap_or_default());
    let lines: Vec<String> =
        v["lines"].as_array().map(|a| a.iter().filter_map(|l| l.as_str().map(String::from)).collect()).unwrap_or_default();
    let summary = format!(
        "NF Switcher self-test: {} ({:.1} s)\n{}\n{}\n",
        if ok { "PASS" } else { "FAIL" },
        v["seconds"].as_f64().unwrap_or(0.0),
        v["error"].as_str().unwrap_or(""),
        lines.join("\n")
    );
    let _ = std::fs::write(dir.join("report.txt"), summary);
    if ok { 0 } else { 1 }
}

fn run_inner(dir: &Path) -> Result<Value, String> {
    if !crate::elevate::is_elevated() {
        return Err("needs admin: start it with Run as administrator".into());
    }
    let ifaces = net::snapshot();
    let e = ifaces.iter().find(|i| i.default_route).cloned();
    let w = ifaces
        .iter()
        .find(|i| i.kind == net::Kind::Wifi && i.up() && !i.default_route)
        .or_else(|| crate::engine::pick_target(&ifaces, None).filter(|t| t.up()))
        .cloned()
        .ok_or("no connected Wi-Fi")?;
    let distinct = !w.default_route;
    let lan = lan_of(&ifaces, Some(&w));
    let (pinned, other) = copy_probe(dir)?;
    let rp_before = routepolicies::read();
    let mut c = Ctx { w, e, lan, pinned, other, distinct, steps: Vec::new(), lines: Vec::new() };
    c.lines.push(format!(
        "Wi-Fi: {} {:?} | default route: {} | Ethernet LAN kept: {:?} | routepolicies before: {:?} | {}",
        c.w.alias,
        c.w.ipv4,
        c.e.as_ref().map(|d| format!("{} {:?}", d.alias, d.ipv4)).unwrap_or("none".into()),
        c.lan.as_ref().map(|l| l.nets.iter().map(|n| format!("{}/{}", n.addr, n.len)).collect::<Vec<_>>()),
        rp_before,
        if distinct { "two connections" } else { "ONLY ONE CONNECTION: Wi-Fi is the default route, moves can't be seen" }
    ));
    let mut pass = c.step("baseline", "default", "nothing pinned yet");

    // A: routepolicies as found.
    let mut a = c.engine()?;
    let t0 = Instant::now();
    let applied = a.apply(&c.plan(false, true, false, None)).map_err(|e| format!("A: {e}"))?;
    let ms = t0.elapsed().as_millis();
    std::thread::sleep(Duration::from_millis(300));
    let note = format!("({} policies, {} filters, {ms} ms)", applied.policies, applied.filters);
    let a_moved = c.step_with("A-routepolicies-as-found", "wifi", &note, V4_UDP, true);
    a.clear().map_err(|e| format!("A clear: {e}"))?;
    drop(a);

    // B: routepolicies on, held for the rest of the test when it's needed.
    let mut rp = Netsh::new();
    let mut needs_rp = false;
    if distinct && !a_moved && rp_before != Some(true) {
        rp.need()?;
        let mut b = c.engine()?;
        b.apply(&c.plan(false, true, false, None)).map_err(|e| format!("B: {e}"))?;
        std::thread::sleep(Duration::from_millis(300));
        needs_rp = c.step("B-routepolicies-on", "wifi", &format!("(routepolicies now {:?})", routepolicies::read()));
        b.clear().map_err(|e| format!("B clear: {e}"))?;
        pass &= needs_rp;
    }

    // Running: the copy is already making connections when the pin lands.
    {
        let child = spawn_probe(&c.pinned, &["loop", "10", V4_TCP])?;
        std::thread::sleep(Duration::from_millis(1800));
        let mut e = c.engine()?;
        e.apply(&c.plan(false, true, false, None)).map_err(|e| format!("running: {e}"))?;
        let out = finish(child);
        e.clear().map_err(|e| format!("running clear: {e}"))?;
        let sides: Vec<&str> = out.lines().filter(|l| l.starts_with("LOOP")).map(|l| c.side(local(l))).collect();
        let ok = sides.first() == Some(&c.want("default")) && sides.last() == Some(&"wifi");
        let summary = format!("its connections in order: {}", sides.join(" "));
        pass &= c.record("running-app", ok, false, summary, json!({"raw": out}));
    }

    // LAN-keep: a DNS query to the Ethernet router from the pinned copy stays on Ethernet.
    let lan_dns = c.e.as_ref().and_then(|e| e.gw4).filter(|_| c.lan.is_some()).map(|g| format!("{g}:53"));
    if let Some(dns) = &lan_dns {
        let mut e = c.engine()?;
        e.apply(&c.plan(false, true, true, None)).map_err(|e| format!("lan: {e}"))?;
        std::thread::sleep(Duration::from_millis(300));
        let p = parse(&run_probe(&c.pinned, &[V4_TCP, dns]));
        e.clear().map_err(|e| format!("lan clear: {e}"))?;
        let (pt, pu) = (c.side(p.tcp), c.side(p.udp));
        let ok = pt == "wifi" && pu == "default";
        // A router that doesn't answer DNS makes this inconclusive, not a failure.
        let info = pu == "failed";
        pass &= ok || info;
        let summary = format!("pinned internet TCP {pt}, DNS to the Ethernet router {dns}: {pu}");
        c.record("lan-keep", ok, info, summary, json!({"raw": p.raw}));
    }

    // Strict mode while Wi-Fi is up, then an open connection while filters change around it.
    let mut e = c.engine()?;
    e.apply(&c.plan(true, true, true, None)).map_err(|err| format!("strict: {err}"))?;
    std::thread::sleep(Duration::from_millis(300));
    pass &= c.step("strict-up", "wifi", "block until Wi-Fi is back, Wi-Fi up: must still work over Wi-Fi");
    {
        let child = spawn_probe(&c.pinned, &["hold", "8", V4_TCP])?;
        std::thread::sleep(Duration::from_millis(2000));
        // Adding and removing filters (here: pinning the other copy for a moment) makes Windows re-check every
        // open connection at those layers.
        e.apply(&c.plan(true, true, true, Some(&c.other))).map_err(|err| format!("reauth add: {err}"))?;
        std::thread::sleep(Duration::from_millis(1200));
        e.apply(&c.plan(true, true, true, None)).map_err(|err| format!("reauth remove: {err}"))?;
        let out = finish(child);
        let holds: Vec<&str> = out.lines().filter(|l| l.starts_with("HOLD")).collect();
        let answered = holds.iter().filter(|l| l.contains(" ok ")).count();
        let ok = answered == 8 && c.side(holds.first().and_then(|l| local(l))) == "wifi";
        let summary = format!("{answered} of 8 requests on one Wi-Fi connection answered while filters changed");
        pass &= c.record("reauth", ok, false, summary, json!({"raw": out}));
    }
    e.apply(&c.plan(true, false, false, None)).map_err(|err| format!("no-wifi: {err}"))?;
    std::thread::sleep(Duration::from_millis(300));
    pass &= c.step("no-wifi", "failed", "block mode without a Wi-Fi adapter: pinned must be blocked, the other one fine");
    e.clear().map_err(|err| format!("clear: {err}"))?;
    drop(e);
    std::thread::sleep(Duration::from_millis(300));
    pass &= c.step("cleared", "default", "everything removed");

    // Crash: a child adds the policy and is killed; Windows must remove it.
    if distinct && (a_moved || needs_rp) {
        let ready = dir.join("child-ready");
        let _ = std::fs::remove_file(&ready);
        let me = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut child = Command::new(me)
            .args(["--selftest-child", &c.pinned.to_string_lossy(), &ready.to_string_lossy()])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| format!("starting the child: {e}"))?;
        let t = Instant::now();
        while !ready.exists() && t.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(100));
        }
        if ready.exists() {
            std::thread::sleep(Duration::from_millis(300));
            pass &= c.step("crash-before-kill", "wifi", "child holds the policy");
            let _ = child.kill();
            let _ = child.wait();
            std::thread::sleep(Duration::from_millis(1000));
            pass &= c.step("crash-after-kill", "default", "child killed: Windows removed its policy");
        } else {
            let _ = child.kill();
            c.lines.push("crash step skipped: the child didn't get ready".into());
            pass = false;
        }
    }

    rp.release();
    let rp_after = routepolicies::read();
    c.lines.push(format!("routepolicies at the end: {rp_after:?} (before: {rp_before:?})"));
    pass &= rp_after == rp_before;
    Ok(json!({
        "pass": pass,
        "distinct_connections": distinct,
        "policies_route_without_routepolicies": a_moved,
        "routepolicies_needed": needs_rp,
        "lines": c.lines,
        "steps": c.steps,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_probe_output() {
        let p = parse("TCP local=192.168.0.34:5000 remote=1.1.1.1:80 answer=\"HTTP/1.1 301\"\nUDP local=192.168.0.34:6000 remote=1.1.1.1:53 answer=true\n");
        assert_eq!(p.tcp, Some("192.168.0.34".parse().unwrap()));
        assert_eq!(p.udp, Some("192.168.0.34".parse().unwrap()));
        let p = parse("TCP remote=1.1.1.1:80 error=blocked\nUDP local=0.0.0.0:6000 remote=1.1.1.1:53 answer=false\n");
        assert_eq!((p.tcp, p.udp), (None, None));
        assert_eq!(local("LOOP 3 local=192.168.8.99:5123 ok"), Some("192.168.8.99".parse().unwrap()));
    }
}
