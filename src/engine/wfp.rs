// HOW NF SWITCHER PUTS AN APP ON WI-FI, AND THE TRADE-OFFS
//
// Steer: a Windows Filtering Platform *connection policy* per pinned .exe (FwpmConnectionPolicyAdd0, Windows 11 24H2+,
//   admin) sends its NEW TCP and UDP connections out of the Wi-Fi interface (its LUID). Windows applies it once,
//   when a connection is set up: no driver, download, proxy, code injection, packet rewrite, route or metric change,
//   so other apps (games on Ethernet) pay nothing per packet. It matches the .exe path, so apps started later are
//   covered without watching processes. A higher-weight policy keeps the Ethernet LAN (router page, NAS) on Ethernet.
// Guard ("Block until Wi-Fi is back"): plain WFP filters, no callout, let a pinned .exe connect only over Wi-Fi,
//   loopback or the Ethernet LAN; everything else fails at once instead of leaking over Ethernet. Laid out as
//   weighted permits over one block (no "not equal" conditions), so Windows re-checking open connections
//   (reauthorization, with the interface sometimes unknown) can't cut the app's Wi-Fi connections.
// Cleanup: everything lives in a dynamic WFP session, so Windows deletes it when NF Switcher exits OR crashes
//   (measured with --selftest, 2026-10-18). Windows ignores the policies unless its global "routepolicies" TCP/IP
//   option is on (measured the same way, build 26200): routepolicies.rs turns it on for this boot only, writes down
//   what it found first, and puts it back.
// Limits: open connections don't move (IPv4 TCP can be reset so the app reconnects); DNS left to Windows goes via
//   the DNS Client service on the default route; helpers with another .exe need their own pin; while Wi-Fi has no
//   IPv6 a pinned app's internet IPv6 is blocked so it uses IPv4; Windows may drop an idle Wi-Fi while Ethernet is up.
// Rejected: WinDivert (third-party kernel driver download, a callout on every packet, NAT that breaks on exit,
//   unmaintained since 2022), Wintun + a user-space TCP/IP stack (all traffic through user space), a SOCKS proxy
//   (proxy-aware apps only, TCP only), global metrics (move every app), DLL injection (out of scope).
use super::{Applied, Engine, EngineError, Net4, Plan};
use crate::config::norm;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// The guard's filters for one app, layer and IP family, from the highest weight down. Within NF Switcher's own
/// sublayer the highest-weight match decides, so a permit beats the block below it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Guard {
    /// Loopback always works.
    PermitLoopback,
    /// The Wi-Fi interface.
    PermitIface(u64),
    /// The Ethernet LAN, over Ethernet (IPv4 only).
    PermitLan(u64, Net4),
    /// An open connection being re-checked by Windows with its interface unknown (FWP_EMPTY) stays open.
    PermitReauthUnknown,
    /// Everything else.
    BlockAll,
}

/// One thing NF Switcher asks Windows for. The app is the normalised path.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Item {
    /// New connections of `app` leave via the interface `luid`; with `lan`, only those to that network
    /// (the higher-weight LAN-keep policy).
    Route { app: String, luid: u64, v6: bool, lan: Option<Net4> },
    /// One guard filter, outbound (ALE_AUTH_CONNECT) or accepted inbound (ALE_AUTH_RECV_ACCEPT).
    Guard { app: String, v6: bool, inbound: bool, kind: Guard },
    /// Block `app`'s outbound IPv6 to internet addresses (2000::/3), so it falls back to IPv4; local IPv6 still works.
    NoIpv6 { app: String },
}

impl Item {
    pub fn app(&self) -> &str {
        match self {
            Item::Route { app, .. } | Item::Guard { app, .. } | Item::NoIpv6 { app } => app,
        }
    }
    pub fn is_policy(&self) -> bool {
        matches!(self, Item::Route { .. })
    }
}

/// What Windows should hold for a plan.
pub fn desired(plan: &Plan) -> BTreeSet<Item> {
    let mut s = BTreeSet::new();
    for a in plan.apps.iter().map(|a| norm(a)) {
        if let Some(t) = &plan.target {
            if plan.v4 {
                s.insert(Item::Route { app: a.clone(), luid: t.luid, v6: false, lan: None });
            }
            if plan.v6 {
                s.insert(Item::Route { app: a.clone(), luid: t.luid, v6: true, lan: None });
            }
        }
        if let Some(l) = &plan.lan {
            for n in &l.nets {
                s.insert(Item::Route { app: a.clone(), luid: l.luid, v6: false, lan: Some(*n) });
            }
        }
        if plan.block_v6 {
            s.insert(Item::NoIpv6 { app: a.clone() });
        }
        if plan.guard {
            for v6 in [false, true] {
                for inbound in [false, true] {
                    let mut g = |kind| s.insert(Item::Guard { app: a.clone(), v6, inbound, kind });
                    g(Guard::PermitLoopback);
                    if let Some(t) = &plan.target {
                        g(Guard::PermitIface(t.luid));
                    }
                    if let (Some(l), false) = (&plan.lan, v6) {
                        for n in &l.nets {
                            g(Guard::PermitLan(l.luid, *n));
                        }
                    }
                    g(Guard::PermitReauthUnknown);
                    g(Guard::BlockAll);
                }
            }
        }
    }
    s
}

/// What to remove and what to add to go from `have` to `want`.
pub fn diff<'a>(have: impl Iterator<Item = &'a Item>, want: &BTreeSet<Item>) -> (Vec<Item>, Vec<Item>) {
    let have: BTreeSet<&Item> = have.collect();
    let remove = have.iter().filter(|i| !want.contains(**i)).map(|i| (*i).clone()).collect();
    let add = want.iter().filter(|i| !have.contains(i)).cloned().collect();
    (remove, add)
}

/// Windows' handle for an added item.
#[derive(Clone, Debug, PartialEq)]
pub enum Handle {
    Policy(windows::core::GUID),
    Filter(u64),
}

#[derive(Debug, PartialEq)]
pub enum AddError {
    /// This one program can't be covered (its .exe is gone); the rest goes on.
    Skip(String),
    /// Windows refused; this change stops and the earlier state stays.
    Fail(String),
}

/// The calls the engine makes, so the logic can be tested with a stand-in instead of the real WFP. Filters are
/// added and removed inside transactions; connection policies are not assumed to take part in them.
pub trait Backend {
    fn begin(&mut self) -> Result<(), String>;
    fn commit(&mut self) -> Result<(), String>;
    fn abort(&mut self);
    fn add(&mut self, it: &Item, path: &str) -> Result<Handle, AddError>;
    fn remove(&mut self, h: &Handle) -> Result<(), String>;
}

/// Windows' "routepolicies" option, behind a trait for the same reason.
pub trait RoutePolicies {
    /// Turns it on if it's off (until the next boot) and remembers that we did.
    fn need(&mut self) -> Result<(), String>;
    /// Puts it back the way it was, if we changed it.
    fn release(&mut self);
}

pub struct WfpEngine<B: Backend, R: RoutePolicies> {
    b: B,
    rp: R,
    have: BTreeMap<Item, Handle>,
}

impl<B: Backend, R: RoutePolicies> WfpEngine<B, R> {
    pub fn new(b: B, rp: R) -> Self {
        WfpEngine { b, rp, have: BTreeMap::new() }
    }

    #[cfg(test)]
    pub fn items(&self) -> impl Iterator<Item = &Item> {
        self.have.keys()
    }

    /// Adds filters in one transaction (all or nothing).
    fn add_filters(&mut self, add: &[(Item, String)], applied: &mut Applied) -> Result<(), EngineError> {
        if add.is_empty() {
            return Ok(());
        }
        self.b.begin().map_err(EngineError::Failed)?;
        let mut got = Vec::new();
        for (it, path) in add {
            match self.b.add(it, path) {
                Ok(h) => got.push((it.clone(), h)),
                Err(AddError::Skip(why)) => skip(applied, path, why),
                Err(AddError::Fail(e)) => {
                    self.b.abort();
                    return Err(EngineError::Failed(e));
                }
            }
        }
        self.b.commit().map_err(EngineError::Failed)?;
        self.have.extend(got);
        Ok(())
    }

    /// The order never lets a guarded app leak: on pin its guard arrives before its policy, on unpin its policy leaves
    /// before its guard (at worst a connection is blocked for a moment, never sent over Ethernet).
    fn change(&mut self, remove: Vec<Item>, add: Vec<(Item, String)>) -> Result<Applied, EngineError> {
        let mut applied = Applied::default();
        let (add_p, add_f): (Vec<_>, Vec<_>) = add.into_iter().partition(|(i, _)| i.is_policy());
        let (rm_p, rm_f): (Vec<_>, Vec<_>) = remove.into_iter().partition(|i| i.is_policy());

        // 1. New filters first (a new pin is guarded before it's steered).
        self.add_filters(&add_f, &mut applied)?;

        // 2. New policies, one by one. On a refusal the ones just added are taken out again.
        let mut got = Vec::new();
        for (it, path) in &add_p {
            match self.b.add(it, path) {
                Ok(h) => got.push((it.clone(), h)),
                Err(AddError::Skip(why)) => skip(&mut applied, path, why),
                Err(AddError::Fail(e)) => {
                    for (_, h) in &got {
                        let _ = self.b.remove(h);
                    }
                    return Err(EngineError::Failed(e));
                }
            }
        }
        self.have.extend(got);

        // 3. Old policies out (an unpinned app is steered no more), then 4. old filters in one transaction.
        for it in rm_p {
            if let Some(h) = self.have.get(&it).cloned() {
                // A policy Windows no longer has (its session restarted) is gone either way.
                let _ = self.b.remove(&h);
                self.have.remove(&it);
            }
        }
        if !rm_f.is_empty() {
            self.b.begin().map_err(EngineError::Failed)?;
            for it in &rm_f {
                if let Some(h) = self.have.get(it) {
                    if let Err(e) = self.b.remove(h) {
                        self.b.abort();
                        return Err(EngineError::Failed(e));
                    }
                }
            }
            self.b.commit().map_err(EngineError::Failed)?;
            for it in &rm_f {
                self.have.remove(it);
            }
        }
        Ok(self.count(applied))
    }

    fn count(&self, mut a: Applied) -> Applied {
        a.policies = self.have.keys().filter(|i| i.is_policy()).count();
        a.filters = self.have.len() - a.policies;
        a
    }
}

fn skip(applied: &mut Applied, path: &str, why: String) {
    if !applied.skipped.iter().any(|(p, _)| p == path) {
        applied.skipped.push((path.to_string(), why));
    }
}

impl<B: Backend, R: RoutePolicies> Engine for WfpEngine<B, R> {
    fn apply(&mut self, plan: &Plan) -> Result<Applied, EngineError> {
        let want = desired(plan);
        // Original paths for FwpmGetAppIdFromFileName0, by normalised key.
        let paths: HashMap<String, String> = plan.apps.iter().map(|p| (norm(p), p.clone())).collect();
        let (remove, add) = diff(self.have.keys(), &want);
        if remove.is_empty() && add.is_empty() {
            return Ok(self.count(Applied::default()));
        }
        if want.iter().any(|i| i.is_policy()) {
            // On before the policies exist, so a connection never meets a policy Windows ignores.
            self.rp.need().map_err(EngineError::Failed)?;
        }
        let add = add.into_iter().map(|i| {
            let p = paths.get(i.app()).cloned().unwrap_or_else(|| i.app().to_string());
            (i, p)
        });
        let r = self.change(remove, add.collect());
        if !self.have.keys().any(|i| i.is_policy()) {
            self.rp.release();
        }
        r
    }

    fn clear(&mut self) -> Result<(), EngineError> {
        let all: Vec<Item> = self.have.keys().cloned().collect();
        let r = self.change(all, Vec::new()).map(|_| ());
        self.rp.release();
        r
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The real backend: fwpuclnt.dll. Needs admin. Everything is added in one dynamic session.

pub mod real {
    use super::{AddError, Backend, Guard, Handle, Item};
    use crate::engine::{EngineError, Net4};
    use windows::core::{w, GUID, HSTRING, PCSTR, PWSTR};
    use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, HANDLE};
    use windows::Win32::NetworkManagement::WindowsFilteringPlatform::*;
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32};
    use windows::Win32::System::Rpc::RPC_C_AUTHN_WINNT;

    type PolicyAdd = unsafe extern "system" fn(
        engine: HANDLE,
        policy: *const FWPM_PROVIDER_CONTEXT3,
        ip_version: FWP_IP_VERSION,
        weight: u64,
        num: u32,
        conds: *const FWPM_FILTER_CONDITION0,
        sd: *const core::ffi::c_void,
    ) -> u32;
    type PolicyDelete = unsafe extern "system" fn(engine: HANDLE, key: *const GUID) -> u32;

    /// Policy weights: the LAN-keep policy outranks the main one (the higher weight wins).
    const WEIGHT_MAIN: u64 = 0x1000_0000;
    const WEIGHT_LAN: u64 = 0x1000_0100;
    /// NL_ADDRESS_TYPE NlatUnicast: multicast and broadcast (local discovery) stay where Windows sends them.
    const NLAT_UNICAST: u8 = 1;

    pub struct RealWfp {
        h: HANDLE,
        /// A fresh sublayer per session, so a previous session still being torn down can't collide with it.
        sublayer: GUID,
        add_policy: PolicyAdd,
        del_policy: PolicyDelete,
    }

    unsafe impl Send for RealWfp {}

    fn check(what: &str, r: u32) -> Result<(), String> {
        if r == 0 { Ok(()) } else { Err(format!("{what} failed: 0x{r:08X}")) }
    }

    impl RealWfp {
        /// Opens a dynamic WFP session. Fails with NeedsAdmin without admin, Unsupported before Windows 11 24H2.
        pub fn open() -> Result<RealWfp, EngineError> {
            unsafe {
                let dll = LoadLibraryExW(w!("fwpuclnt.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32)
                    .map_err(|e| EngineError::Unsupported(e.message()))?;
                let add = GetProcAddress(dll, PCSTR(b"FwpmConnectionPolicyAdd0\0".as_ptr()));
                let del = GetProcAddress(dll, PCSTR(b"FwpmConnectionPolicyDeleteByKey0\0".as_ptr()));
                let (Some(add), Some(del)) = (add, del) else {
                    return Err(EngineError::Unsupported("Windows 11 24H2 or later is needed".into()));
                };
                let name = HSTRING::from("NF Switcher");
                let session = FWPM_SESSION0 {
                    displayData: FWPM_DISPLAY_DATA0 { name: PWSTR(name.as_ptr() as *mut _), description: PWSTR::null() },
                    flags: FWPM_SESSION_FLAG_DYNAMIC,
                    txnWaitTimeoutInMSec: 3000,
                    ..Default::default()
                };
                let mut h = HANDLE::default();
                let r = FwpmEngineOpen0(None, RPC_C_AUTHN_WINNT, None, Some(&session), &mut h);
                if r == ERROR_ACCESS_DENIED.0 {
                    return Err(EngineError::NeedsAdmin);
                }
                check("FwpmEngineOpen0", r).map_err(EngineError::Failed)?;
                let me = RealWfp {
                    h,
                    sublayer: GUID::new().map_err(|e| EngineError::Failed(e.message()))?,
                    add_policy: std::mem::transmute::<unsafe extern "system" fn() -> isize, PolicyAdd>(add),
                    del_policy: std::mem::transmute::<unsafe extern "system" fn() -> isize, PolicyDelete>(del),
                };
                let sub = FWPM_SUBLAYER0 {
                    subLayerKey: me.sublayer,
                    displayData: FWPM_DISPLAY_DATA0 { name: PWSTR(name.as_ptr() as *mut _), description: PWSTR::null() },
                    weight: 0xF000,
                    ..Default::default()
                };
                let r = FwpmSubLayerAdd0(me.h, &sub, None);
                if r == ERROR_ACCESS_DENIED.0 {
                    return Err(EngineError::NeedsAdmin);
                }
                check("FwpmSubLayerAdd0", r).map_err(EngineError::Failed)?;
                Ok(me)
            }
        }

        /// Adds a filter with these conditions to NF Switcher's sublayer.
        unsafe fn filter(&mut self, layer: GUID, weight: u8, permit: bool, conds: &mut [FWPM_FILTER_CONDITION0]) -> Result<Handle, AddError> {
            let name = HSTRING::from(if permit { "NF Switcher: allow" } else { "NF Switcher: block" });
            let filter = FWPM_FILTER0 {
                displayData: FWPM_DISPLAY_DATA0 { name: PWSTR(name.as_ptr() as *mut _), description: PWSTR::null() },
                layerKey: layer,
                subLayerKey: self.sublayer,
                weight: FWP_VALUE0 { r#type: FWP_UINT8, Anonymous: FWP_VALUE0_0 { uint8: weight } },
                numFilterConditions: conds.len() as u32,
                filterCondition: conds.as_mut_ptr(),
                action: FWPM_ACTION0 { r#type: if permit { FWP_ACTION_PERMIT } else { FWP_ACTION_BLOCK }, ..Default::default() },
                ..Default::default()
            };
            let mut id_out = 0u64;
            let r = FwpmFilterAdd0(self.h, &filter, None, Some(&mut id_out));
            check("FwpmFilterAdd0", r).map_err(AddError::Fail)?;
            Ok(Handle::Filter(id_out))
        }

        /// Adds a connection policy: matching connections leave via `luid`.
        unsafe fn policy(&mut self, v6: bool, luid: u64, weight: u64, conds: &[FWPM_FILTER_CONDITION0]) -> Result<Handle, AddError> {
            let mut luid = luid;
            let mut setting = FWPM_NETWORK_CONNECTION_POLICY_SETTING0 {
                r#type: FWP_NETWORK_CONNECTION_POLICY_NEXT_HOP_INTERFACE,
                value: FWP_VALUE0 { r#type: FWP_UINT64, Anonymous: FWP_VALUE0_0 { uint64: &mut luid } },
            };
            let mut settings = FWPM_NETWORK_CONNECTION_POLICY_SETTINGS0 { numSettings: 1, settings: &mut setting };
            let key = GUID::new().map_err(|e| AddError::Fail(e.message()))?;
            let name = HSTRING::from("NF Switcher: app to Wi-Fi");
            let ctx = FWPM_PROVIDER_CONTEXT3 {
                providerContextKey: key,
                displayData: FWPM_DISPLAY_DATA0 { name: PWSTR(name.as_ptr() as *mut _), description: PWSTR::null() },
                r#type: FWPM_NETWORK_CONNECTION_POLICY_CONTEXT,
                Anonymous: FWPM_PROVIDER_CONTEXT3_0 { networkConnectionPolicy: &mut settings },
                ..Default::default()
            };
            let ver = if v6 { FWP_IP_VERSION_V6 } else { FWP_IP_VERSION_V4 };
            let r = (self.add_policy)(self.h, &ctx, ver, weight, conds.len() as u32, conds.as_ptr(), std::ptr::null());
            check("FwpmConnectionPolicyAdd0", r).map_err(AddError::Fail)?;
            Ok(Handle::Policy(key))
        }
    }

    impl Drop for RealWfp {
        fn drop(&mut self) {
            // Closing a dynamic session removes everything it added.
            unsafe {
                FwpmEngineClose0(self.h);
            }
        }
    }

    /// The program's AppId blob (its NT path). Freed on drop.
    struct AppId(*mut FWP_BYTE_BLOB);

    impl AppId {
        fn of(path: &str) -> Result<AppId, AddError> {
            let mut blob: *mut FWP_BYTE_BLOB = std::ptr::null_mut();
            let r = unsafe { FwpmGetAppIdFromFileName0(&HSTRING::from(path), &mut blob) };
            if r != 0 || blob.is_null() {
                return Err(AddError::Skip(format!("can't read the program (0x{r:08X})")));
            }
            Ok(AppId(blob))
        }
    }

    impl Drop for AppId {
        fn drop(&mut self) {
            unsafe { FwpmFreeMemory0(&mut self.0 as *mut *mut FWP_BYTE_BLOB as *mut *mut core::ffi::c_void) };
        }
    }

    fn cond(field: GUID, m: FWP_MATCH_TYPE, t: FWP_DATA_TYPE, v: FWP_CONDITION_VALUE0_0) -> FWPM_FILTER_CONDITION0 {
        FWPM_FILTER_CONDITION0 { fieldKey: field, matchType: m, conditionValue: FWP_CONDITION_VALUE0 { r#type: t, Anonymous: v } }
    }

    fn cond_app(id: &AppId) -> FWPM_FILTER_CONDITION0 {
        cond(FWPM_CONDITION_ALE_APP_ID, FWP_MATCH_EQUAL, FWP_BYTE_BLOB_TYPE, FWP_CONDITION_VALUE0_0 { byteBlob: id.0 })
    }

    fn cond_flags(m: FWP_MATCH_TYPE, flags: u32) -> FWPM_FILTER_CONDITION0 {
        cond(FWPM_CONDITION_FLAGS, m, FWP_UINT32, FWP_CONDITION_VALUE0_0 { uint32: flags })
    }

    /// IP_LOCAL_INTERFACE == luid. The u64 must stay alive until the add call returns.
    fn cond_iface(luid: &mut u64) -> FWPM_FILTER_CONDITION0 {
        cond(FWPM_CONDITION_IP_LOCAL_INTERFACE, FWP_MATCH_EQUAL, FWP_UINT64, FWP_CONDITION_VALUE0_0 { uint64: luid })
    }

    /// IP_REMOTE_ADDRESS in an IPv4 network (WFP takes IPv4 addresses in host byte order).
    fn cond_net(m: &mut FWP_V4_ADDR_AND_MASK) -> FWPM_FILTER_CONDITION0 {
        cond(FWPM_CONDITION_IP_REMOTE_ADDRESS, FWP_MATCH_EQUAL, FWP_V4_ADDR_MASK, FWP_CONDITION_VALUE0_0 { v4AddrMask: m })
    }

    fn v4mask(n: &Net4) -> FWP_V4_ADDR_AND_MASK {
        FWP_V4_ADDR_AND_MASK { addr: u32::from(n.addr), mask: Net4::mask(n.len) }
    }

    impl Backend for RealWfp {
        fn begin(&mut self) -> Result<(), String> {
            check("FwpmTransactionBegin0", unsafe { FwpmTransactionBegin0(self.h, 0) })
        }

        fn commit(&mut self) -> Result<(), String> {
            check("FwpmTransactionCommit0", unsafe { FwpmTransactionCommit0(self.h) })
        }

        fn abort(&mut self) {
            unsafe {
                FwpmTransactionAbort0(self.h);
            }
        }

        fn add(&mut self, it: &Item, path: &str) -> Result<Handle, AddError> {
            let id = AppId::of(path)?;
            unsafe {
                match it {
                    Item::Route { luid, v6, lan: None, .. } => {
                        let conds = [
                            cond_app(&id),
                            cond_flags(FWP_MATCH_FLAGS_NONE_SET, FWP_CONDITION_FLAG_IS_LOOPBACK),
                            cond(
                                FWPM_CONDITION_IP_DESTINATION_ADDRESS_TYPE,
                                FWP_MATCH_EQUAL,
                                FWP_UINT8,
                                FWP_CONDITION_VALUE0_0 { uint8: NLAT_UNICAST },
                            ),
                        ];
                        self.policy(*v6, *luid, WEIGHT_MAIN, &conds)
                    }
                    Item::Route { luid, lan: Some(n), .. } => {
                        let mut m = v4mask(n);
                        let conds = [cond_app(&id), cond_net(&mut m)];
                        self.policy(false, *luid, WEIGHT_LAN, &conds)
                    }
                    Item::NoIpv6 { .. } => {
                        let mut global = FWP_V6_ADDR_AND_MASK { addr: [0x20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], prefixLength: 3 };
                        let mut conds = [
                            cond_app(&id),
                            cond_flags(FWP_MATCH_FLAGS_NONE_SET, FWP_CONDITION_FLAG_IS_LOOPBACK),
                            cond(FWPM_CONDITION_IP_REMOTE_ADDRESS, FWP_MATCH_EQUAL, FWP_V6_ADDR_MASK, FWP_CONDITION_VALUE0_0 { v6AddrMask: &mut global }),
                        ];
                        self.filter(FWPM_LAYER_ALE_AUTH_CONNECT_V6, 8, false, &mut conds)
                    }
                    Item::Guard { v6, inbound, kind, .. } => {
                        let layer = match (*v6, *inbound) {
                            (false, false) => FWPM_LAYER_ALE_AUTH_CONNECT_V4,
                            (true, false) => FWPM_LAYER_ALE_AUTH_CONNECT_V6,
                            (false, true) => FWPM_LAYER_ALE_AUTH_RECV_ACCEPT_V4,
                            (true, true) => FWPM_LAYER_ALE_AUTH_RECV_ACCEPT_V6,
                        };
                        match kind {
                            Guard::PermitLoopback => {
                                let mut c = [cond_app(&id), cond_flags(FWP_MATCH_FLAGS_ALL_SET, FWP_CONDITION_FLAG_IS_LOOPBACK)];
                                self.filter(layer, 15, true, &mut c)
                            }
                            Guard::PermitIface(luid) => {
                                let mut l = *luid;
                                let mut c = [cond_app(&id), cond_iface(&mut l)];
                                self.filter(layer, 14, true, &mut c)
                            }
                            Guard::PermitLan(luid, n) => {
                                let mut l = *luid;
                                let mut m = v4mask(n);
                                let mut c = [cond_app(&id), cond_iface(&mut l), cond_net(&mut m)];
                                self.filter(layer, 13, true, &mut c)
                            }
                            Guard::PermitReauthUnknown => {
                                // "Fields of type FWP_EMPTY can be matched with FWP_MATCH_EQUAL" (ALE reauthorization docs).
                                let mut c = [
                                    cond_app(&id),
                                    cond_flags(FWP_MATCH_FLAGS_ALL_SET, FWP_CONDITION_FLAG_IS_REAUTHORIZE),
                                    cond(FWPM_CONDITION_IP_LOCAL_INTERFACE, FWP_MATCH_EQUAL, FWP_EMPTY, FWP_CONDITION_VALUE0_0::default()),
                                ];
                                self.filter(layer, 11, true, &mut c)
                            }
                            Guard::BlockAll => {
                                let mut c = [cond_app(&id)];
                                self.filter(layer, 10, false, &mut c)
                            }
                        }
                    }
                }
            }
        }

        fn remove(&mut self, h: &Handle) -> Result<(), String> {
            match h {
                Handle::Policy(key) => check("FwpmConnectionPolicyDeleteByKey0", unsafe { (self.del_policy)(self.h, key) }),
                Handle::Filter(id) => match unsafe { FwpmFilterDeleteById0(self.h, *id) } {
                    // FWP_E_FILTER_NOT_FOUND / FWP_E_NOT_FOUND: someone else already removed it; it's gone either way.
                    0x8032_0003 | 0x8032_0008 => Ok(()),
                    r => check("FwpmFilterDeleteById0", r),
                },
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn struct_sizes_match_the_windows_headers() {
            // x64 sizes from fwpmtypes.h; a mismatch would corrupt the call into fwpuclnt.dll.
            assert_eq!(std::mem::size_of::<FWP_VALUE0>(), 16);
            assert_eq!(std::mem::size_of::<FWPM_PROVIDER_CONTEXT3>(), 88);
            assert_eq!(std::mem::size_of::<FWPM_FILTER_CONDITION0>(), 40);
            assert_eq!(std::mem::size_of::<FWPM_NETWORK_CONNECTION_POLICY_SETTING0>(), 24);
        }

        #[test]
        fn ipv4_networks_are_in_host_byte_order() {
            let m = v4mask(&Net4::of(std::net::Ipv4Addr::new(192, 168, 0, 113), 24));
            assert_eq!(m.addr, 0xC0A8_0000);
            assert_eq!(m.mask, 0xFFFF_FF00);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Lan, Target};
    use std::net::Ipv4Addr;

    /// Stand-in for WFP: filters are transactional, policies take effect at once. Records calls; can be told to fail.
    #[derive(Default)]
    pub struct Fake {
        pub log: Vec<String>,
        pub live: BTreeMap<u64, Item>,
        pub next: u64,
        pub fail_on: Option<String>,
        pub missing: Vec<String>,
        in_txn: bool,
        pending: Vec<(bool, u64, Option<Item>)>,
    }

    impl Backend for Fake {
        fn begin(&mut self) -> Result<(), String> {
            self.log.push("begin".into());
            self.in_txn = true;
            self.pending.clear();
            Ok(())
        }
        fn commit(&mut self) -> Result<(), String> {
            self.log.push("commit".into());
            self.in_txn = false;
            for (add, id, it) in self.pending.drain(..) {
                if add {
                    self.live.insert(id, it.unwrap());
                } else {
                    self.live.remove(&id);
                }
            }
            Ok(())
        }
        fn abort(&mut self) {
            self.log.push("abort".into());
            self.in_txn = false;
            self.pending.clear();
        }
        fn add(&mut self, it: &Item, path: &str) -> Result<Handle, AddError> {
            if self.missing.iter().any(|m| m == path) {
                return Err(AddError::Skip("gone".into()));
            }
            if self.fail_on.as_deref() == Some(path) && it.is_policy() == !self.in_txn {
                return Err(AddError::Fail("refused".into()));
            }
            self.next += 1;
            self.log.push(format!("add {}", if it.is_policy() { "policy" } else { "filter" }));
            if self.in_txn {
                self.pending.push((true, self.next, Some(it.clone())));
            } else {
                self.live.insert(self.next, it.clone());
            }
            Ok(Handle::Filter(self.next))
        }
        fn remove(&mut self, h: &Handle) -> Result<(), String> {
            let Handle::Filter(id) = h else { unreachable!() };
            self.log.push(format!("remove {id}"));
            if self.in_txn {
                self.pending.push((false, *id, None));
            } else {
                self.live.remove(id);
            }
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeRp {
        on: bool,
        changes: u32,
    }
    impl RoutePolicies for FakeRp {
        fn need(&mut self) -> Result<(), String> {
            if !self.on {
                self.on = true;
                self.changes += 1;
            }
            Ok(())
        }
        fn release(&mut self) {
            if self.on {
                self.on = false;
                self.changes += 1;
            }
        }
    }

    fn target(luid: u64, ipv6: bool) -> Target {
        Target { luid, index: luid as u32, name: "Wi-Fi".into(), ipv4: vec![], ipv6 }
    }

    fn plan(apps: &[&str], t: Option<Target>, strict: bool) -> Plan {
        let ipv6 = t.as_ref().is_some_and(|t| t.ipv6);
        Plan {
            apps: apps.iter().map(|s| s.to_string()).collect(),
            v4: true,
            v6: ipv6 || strict,
            block_v6: !ipv6 && !strict && t.is_some(),
            guard: strict,
            target: t,
            lan: None,
        }
    }

    fn lan() -> Lan {
        Lan { luid: 1, nets: vec![crate::engine::Net4::of(Ipv4Addr::new(192, 168, 0, 1), 24)] }
    }

    #[test]
    fn desired_items_per_plan() {
        let d = desired(&plan(&[r"C:\B\brave.exe"], Some(target(7, false)), false));
        assert_eq!(d.len(), 2); // route v4 + internet-IPv6 block
        let d = desired(&plan(&[r"C:\B\brave.exe"], Some(target(7, true)), false));
        assert!(d.len() == 2 && d.iter().all(|i| i.is_policy()));
        // Strict: 2 routes + 4 guard sets (v4/v6 x connect/accept) of loopback, Wi-Fi, reauth and block.
        let d = desired(&plan(&[r"C:\B\brave.exe"], Some(target(7, true)), true));
        assert_eq!(d.len(), 2 + 4 * 4);
        assert!(!d.iter().any(|i| matches!(i, Item::NoIpv6 { .. })));
        // No adapter at all: only loopback, reauth and block.
        let d = desired(&plan(&[r"C:\B\brave.exe"], None, true));
        assert_eq!(d.len(), 4 * 3);
        // LAN-keep adds a policy, and in strict mode an IPv4 permit per layer.
        let mut p = plan(&[r"C:\B\brave.exe"], Some(target(7, true)), true);
        p.lan = Some(lan());
        let d = desired(&p);
        assert_eq!(d.len(), 3 + 4 * 4 + 2);
        assert!(d.contains(&Item::Route { app: norm(r"C:\B\brave.exe"), luid: 1, v6: false, lan: Some(lan().nets[0]) }));
    }

    #[test]
    fn a_new_pin_is_guarded_before_it_is_steered() {
        let mut e = WfpEngine::new(Fake::default(), FakeRp::default());
        e.apply(&plan(&[r"C:\B\brave.exe"], Some(target(7, true)), true)).unwrap();
        let first_policy = e.b.log.iter().position(|l| l == "add policy").unwrap();
        let commit = e.b.log.iter().position(|l| l == "commit").unwrap();
        assert!(commit < first_policy, "{:?}", e.b.log);
        assert_eq!(e.b.live.len(), 18);
    }

    #[test]
    fn apply_adds_then_only_changes_the_difference() {
        let mut e = WfpEngine::new(Fake::default(), FakeRp::default());
        let p = plan(&[r"C:\B\brave.exe", r"C:\S\spotify.exe"], Some(target(7, true)), false);
        let a = e.apply(&p).unwrap();
        assert_eq!(a.policies, 4);
        assert!(e.rp.on);
        let n = e.b.log.len();
        e.apply(&p).unwrap();
        assert_eq!(e.b.log.len(), n, "the same plan again makes no calls");
        e.apply(&plan(&[r"C:\B\brave.exe"], Some(target(7, true)), false)).unwrap();
        assert_eq!(e.b.live.len(), 2);
    }

    #[test]
    fn wifi_luid_change_moves_the_policies() {
        let mut e = WfpEngine::new(Fake::default(), FakeRp::default());
        e.apply(&plan(&[r"C:\B\brave.exe"], Some(target(7, true)), false)).unwrap();
        e.apply(&plan(&[r"C:\B\brave.exe"], Some(target(9, true)), false)).unwrap();
        assert!(e.b.live.values().all(|i| matches!(i, Item::Route { luid: 9, .. })));
        assert_eq!(e.b.live.len(), 2);
    }

    #[test]
    fn a_refused_policy_takes_its_batch_back_out() {
        let mut e = WfpEngine::new(Fake::default(), FakeRp::default());
        e.apply(&plan(&[r"C:\B\brave.exe"], Some(target(7, true)), false)).unwrap();
        e.b.fail_on = Some(r"C:\S\spotify.exe".into());
        let r = e.apply(&plan(&[r"C:\B\brave.exe", r"C:\S\spotify.exe"], Some(target(7, true)), false));
        assert!(matches!(r, Err(EngineError::Failed(_))));
        // Brave's two policies stay; none of Spotify's is left behind in Windows or in the map.
        assert_eq!(e.b.live.len(), 2);
        assert_eq!(e.items().count(), 2);
    }

    #[test]
    fn a_refused_filter_rolls_the_transaction_back() {
        let mut e = WfpEngine::new(Fake::default(), FakeRp::default());
        e.b.fail_on = Some(r"C:\B\brave.exe".into());
        let r = e.apply(&plan(&[r"C:\B\brave.exe"], Some(target(7, true)), true));
        assert!(r.is_err());
        assert_eq!(e.b.log.last().unwrap(), "abort");
        assert!(e.b.live.is_empty() && e.items().count() == 0);
    }

    #[test]
    fn a_missing_exe_is_skipped_not_fatal() {
        let mut e = WfpEngine::new(Fake::default(), FakeRp::default());
        e.b.missing = vec![r"C:\Gone\x.exe".into()];
        let a = e.apply(&plan(&[r"C:\B\brave.exe", r"C:\Gone\x.exe"], Some(target(7, true)), false)).unwrap();
        assert_eq!(a.skipped.len(), 1);
        assert_eq!(a.policies, 2);
    }

    #[test]
    fn clear_and_empty_plans_remove_everything_and_release_routepolicies() {
        let mut e = WfpEngine::new(Fake::default(), FakeRp::default());
        e.apply(&plan(&[r"C:\B\brave.exe"], Some(target(7, false)), true)).unwrap();
        assert!(!e.b.live.is_empty() && e.rp.on);
        e.clear().unwrap();
        assert!(e.b.live.is_empty() && !e.rp.on);
        e.apply(&plan(&[r"C:\B\brave.exe"], Some(target(7, false)), false)).unwrap();
        e.apply(&Plan::default()).unwrap();
        assert!(e.b.live.is_empty() && !e.rp.on);
    }

    #[test]
    fn a_guard_without_any_policy_leaves_routepolicies_alone() {
        let mut e = WfpEngine::new(Fake::default(), FakeRp::default());
        e.apply(&plan(&[r"C:\B\brave.exe"], None, true)).unwrap();
        assert_eq!(e.b.live.len(), 12);
        assert_eq!(e.rp.changes, 0);
    }
}
