// The machine's Wi-Fi and Ethernet adapters, read with IP Helper (no admin needed, nothing changed), and change
// notifications from Windows itself (NotifyIpInterfaceChange and friends), so nothing polls.
use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use windows::core::GUID;
use windows::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, HANDLE, NO_ERROR};
use windows::Win32::NetworkManagement::IpHelper::*;
use windows::Win32::NetworkManagement::Ndis::{IfOperStatusUp, NET_LUID_LH};
use windows::Win32::NetworkManagement::WiFi::*;
use windows::Win32::Networking::WinSock::{
    ADDRESS_FAMILY, AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR, SOCKADDR_IN, SOCKADDR_IN6, SOCKET_ADDRESS,
};

pub const IF_TYPE_ETHERNET: u32 = 6;
pub const IF_TYPE_WIFI: u32 = 71;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Ethernet,
    Wifi,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Up, with an address and a gateway.
    Connected,
    /// Up, but no gateway or only a self-assigned address: no internet through it.
    Limited,
    Disconnected,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Iface {
    /// IPv4 interface index (also used for IPv6 when the adapter has one index).
    pub index: u32,
    pub index6: u32,
    pub luid: u64,
    /// AdapterName, "{GUID}".
    pub guid: String,
    /// "Wi-Fi", "Ethernet 2"...
    pub alias: String,
    /// "Intel(R) Wi-Fi 6E AX211 160MHz"...
    pub desc: String,
    pub kind: Kind,
    pub status: Status,
    pub ipv4: Vec<Ipv4Addr>,
    /// The IPv4 networks the adapter is directly on (address and on-link prefix length).
    pub nets: Vec<(Ipv4Addr, u8)>,
    /// Global addresses only (no fe80::).
    pub ipv6: Vec<Ipv6Addr>,
    pub gw4: Option<Ipv4Addr>,
    pub gw6: Option<Ipv6Addr>,
    pub metric4: u32,
    pub speed: u64,
    pub ssid: Option<String>,
    /// The Wi-Fi profile it's connected with (what "Connect Wi-Fi" uses again).
    pub profile: Option<String>,
    /// Windows' best interface towards the internet (IPv4), i.e. what "the default route" means right now.
    pub default_route: bool,
}

impl Iface {
    pub fn up(&self) -> bool {
        self.status == Status::Connected
    }
}

pub fn status_of(oper_up: bool, ipv4: &[Ipv4Addr], ipv6: &[Ipv6Addr], gw4: Option<Ipv4Addr>, gw6: Option<Ipv6Addr>) -> Status {
    if !oper_up {
        return Status::Disconnected;
    }
    let real4 = ipv4.iter().any(|a| !a.is_link_local() && !a.is_unspecified());
    if (real4 && gw4.is_some()) || (!ipv6.is_empty() && gw6.is_some()) {
        Status::Connected
    } else {
        Status::Limited
    }
}

/// The Wi-Fi adapter NF Switcher routes pinned apps through: the one picked in Settings if it exists, else the
/// first connected one, else the first one.
pub fn pick_wifi<'a>(ifaces: &'a [Iface], preferred: Option<&str>) -> Option<&'a Iface> {
    let wifi = || ifaces.iter().filter(|i| i.kind == Kind::Wifi);
    if let Some(p) = preferred {
        if let Some(i) = wifi().find(|i| i.guid.eq_ignore_ascii_case(p)) {
            return Some(i);
        }
    }
    wifi().find(|i| i.up()).or_else(|| wifi().next())
}

/// The adapter shown as "Ethernet": the default-route one if it's Ethernet, else the first connected Ethernet.
pub fn pick_ethernet(ifaces: &[Iface]) -> Option<&Iface> {
    let eth = || ifaces.iter().filter(|i| i.kind == Kind::Ethernet);
    eth().find(|i| i.default_route).or_else(|| eth().find(|i| i.up())).or_else(|| eth().next())
}

unsafe fn sockaddr(sa: &SOCKET_ADDRESS) -> Option<std::net::IpAddr> {
    if sa.lpSockaddr.is_null() {
        return None;
    }
    let fam: ADDRESS_FAMILY = (*sa.lpSockaddr).sa_family;
    if fam == AF_INET {
        let v4 = &*(sa.lpSockaddr as *const SOCKADDR_IN);
        Some(Ipv4Addr::from(u32::from_be(v4.sin_addr.S_un.S_addr)).into())
    } else if fam == AF_INET6 {
        let v6 = &*(sa.lpSockaddr as *const SOCKADDR_IN6);
        Some(Ipv6Addr::from(v6.sin6_addr.u.Byte).into())
    } else {
        None
    }
}

pub fn guid_string(g: &GUID) -> String {
    let d = g.data4;
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        g.data1, g.data2, g.data3, d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]
    )
}

unsafe fn pwstr(p: windows::core::PWSTR) -> String {
    if p.is_null() { String::new() } else { p.to_string().unwrap_or_default() }
}

fn is_hardware(luid: NET_LUID_LH) -> bool {
    let mut row = MIB_IF_ROW2 { InterfaceLuid: luid, ..Default::default() };
    // Bit 0 of InterfaceAndOperStatusFlags is HardwareInterface: skips Hyper-V, VPN and Wi-Fi Direct adapters.
    unsafe { GetIfEntry2(&mut row) == NO_ERROR && row.InterfaceAndOperStatusFlags._bitfield & 1 != 0 }
}

/// Windows' best interface towards the internet for IPv4 (no packet is sent).
pub fn default_route_index() -> Option<u32> {
    let mut dst = SOCKADDR_IN { sin_family: AF_INET, ..Default::default() };
    dst.sin_addr.S_un.S_addr = u32::from(Ipv4Addr::new(1, 1, 1, 1)).to_be();
    let mut idx = 0u32;
    let r = unsafe { GetBestInterfaceEx(&dst as *const SOCKADDR_IN as *const SOCKADDR, &mut idx) };
    (r == NO_ERROR.0).then_some(idx)
}

/// Current Wi-Fi network names (SSID, profile) by adapter GUID. Windows 11 may require location access for this;
/// without it the map is just empty and the UI shows "Wi-Fi".
pub fn wifi_ssids() -> HashMap<String, (String, String)> {
    let mut out = HashMap::new();
    unsafe {
        let mut ver = 0u32;
        let mut h = HANDLE::default();
        if WlanOpenHandle(2, None, &mut ver, &mut h) != 0 {
            return out;
        }
        let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
        if WlanEnumInterfaces(h, None, &mut list) == 0 && !list.is_null() {
            let n = (*list).dwNumberOfItems as usize;
            let items = std::slice::from_raw_parts((*list).InterfaceInfo.as_ptr(), n);
            for it in items {
                let mut size = 0u32;
                let mut data: *mut core::ffi::c_void = std::ptr::null_mut();
                let r = WlanQueryInterface(
                    h,
                    &it.InterfaceGuid,
                    wlan_intf_opcode_current_connection,
                    None,
                    &mut size,
                    &mut data,
                    None,
                );
                if r == 0 && !data.is_null() {
                    let attr = &*(data as *const WLAN_CONNECTION_ATTRIBUTES);
                    let s = &attr.wlanAssociationAttributes.dot11Ssid;
                    let len = (s.uSSIDLength as usize).min(32);
                    if len > 0 {
                        let ssid = String::from_utf8_lossy(&s.ucSSID[..len]).into_owned();
                        let p = &attr.strProfileName;
                        let profile = String::from_utf16_lossy(&p[..p.iter().position(|&c| c == 0).unwrap_or(p.len())]);
                        out.insert(guid_string(&it.InterfaceGuid), (ssid, profile));
                    }
                    WlanFreeMemory(data);
                }
            }
            WlanFreeMemory(list as *const _);
        }
        WlanCloseHandle(h, None);
    }
    out
}

/// Every physical Wi-Fi and Ethernet adapter, connected or not.
pub fn snapshot() -> Vec<Iface> {
    let flags = GAA_FLAG_INCLUDE_GATEWAYS | GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let mut size: u32 = 16 * 1024;
    let mut buf: Vec<u64>;
    loop {
        buf = vec![0u64; (size as usize).div_ceil(8)];
        let r = unsafe { GetAdaptersAddresses(AF_UNSPEC.0 as u32, flags, None, Some(buf.as_mut_ptr() as *mut _), &mut size) };
        if r == ERROR_BUFFER_OVERFLOW.0 {
            continue;
        }
        if r != NO_ERROR.0 {
            return Vec::new();
        }
        break;
    }
    let best = default_route_index();
    let ssids = wifi_ssids();
    let mut out = Vec::new();
    let mut p = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
    unsafe {
        while !p.is_null() {
            let a = &*p;
            p = a.Next;
            let kind = match a.IfType {
                IF_TYPE_WIFI => Kind::Wifi,
                IF_TYPE_ETHERNET => Kind::Ethernet,
                _ => continue,
            };
            if !is_hardware(a.Luid) {
                continue;
            }
            let (mut ipv4, mut ipv6, mut nets) = (Vec::new(), Vec::new(), Vec::new());
            let mut u = a.FirstUnicastAddress;
            while !u.is_null() {
                match sockaddr(&(*u).Address) {
                    Some(std::net::IpAddr::V4(v)) => {
                        ipv4.push(v);
                        nets.push((v, (*u).OnLinkPrefixLength));
                    }
                    Some(std::net::IpAddr::V6(v)) if (v.segments()[0] & 0xffc0) != 0xfe80 => ipv6.push(v),
                    _ => {}
                }
                u = (*u).Next;
            }
            let (mut gw4, mut gw6) = (None, None);
            let mut g = a.FirstGatewayAddress;
            while !g.is_null() {
                match sockaddr(&(*g).Address) {
                    Some(std::net::IpAddr::V4(v)) if gw4.is_none() => gw4 = Some(v),
                    Some(std::net::IpAddr::V6(v)) if gw6.is_none() => gw6 = Some(v),
                    _ => {}
                }
                g = (*g).Next;
            }
            let index = a.Anonymous1.Anonymous.IfIndex;
            let guid = a.AdapterName.to_string().unwrap_or_default();
            let (ssid, profile) = match ssids.get(&guid.to_uppercase()).cloned() {
                Some((s, p)) => (Some(s), (!p.is_empty()).then_some(p)),
                None => (None, None),
            };
            out.push(Iface {
                index,
                index6: a.Ipv6IfIndex,
                luid: a.Luid.Value,
                status: status_of(a.OperStatus == IfOperStatusUp, &ipv4, &ipv6, gw4, gw6),
                guid,
                alias: pwstr(a.FriendlyName),
                desc: pwstr(a.Description),
                kind,
                ipv4,
                nets,
                ipv6,
                gw4,
                gw6,
                metric4: a.Ipv4Metric,
                speed: a.ReceiveLinkSpeed,
                ssid,
                profile,
                default_route: best == Some(index) && index != 0,
            });
        }
    }
    out
}

/// Connects a Wi-Fi adapter to a network Windows already has a saved profile for, as clicking it in Windows' Wi-Fi
/// list does: `prefer` (the network it was last on) if it's in range, else the strongest saved one. Returns the
/// profile used. The connection itself completes in the background; the adapter notifications report it.
pub fn wifi_connect(adapter_guid: &str, prefer: Option<&str>) -> Result<String, String> {
    let g = GUID::try_from(adapter_guid.trim_matches(|c| c == '{' || c == '}')).map_err(|_| "bad adapter id".to_string())?;
    unsafe {
        let mut ver = 0u32;
        let mut h = HANDLE::default();
        if WlanOpenHandle(2, None, &mut ver, &mut h) != 0 {
            return Err("the Wi-Fi service isn't available".into());
        }
        let mut list: *mut WLAN_AVAILABLE_NETWORK_LIST = std::ptr::null_mut();
        let r = WlanGetAvailableNetworkList(h, &g, 0, None, &mut list);
        if r != 0 || list.is_null() {
            WlanCloseHandle(h, None);
            return Err(format!("listing Wi-Fi networks failed ({r})"));
        }
        let nets = std::slice::from_raw_parts((*list).Network.as_ptr(), (*list).dwNumberOfItems as usize);
        let mut best: Option<(String, bool, u32)> = None;
        for n in nets {
            if n.dwFlags & WLAN_AVAILABLE_NETWORK_HAS_PROFILE == 0 || !n.bNetworkConnectable.as_bool() {
                continue;
            }
            let p = &n.strProfileName;
            let name = String::from_utf16_lossy(&p[..p.iter().position(|&c| c == 0).unwrap_or(p.len())]);
            if name.is_empty() {
                continue;
            }
            let cand = (name.clone(), prefer == Some(name.as_str()), n.wlanSignalQuality);
            let better = match &best {
                None => true,
                Some((_, bp, bq)) => (cand.1, cand.2) > (*bp, *bq),
            };
            if better {
                best = Some(cand);
            }
        }
        WlanFreeMemory(list as *const _);
        let Some((profile, _, _)) = best else {
            WlanCloseHandle(h, None);
            return Err("no saved Wi-Fi network in range".into());
        };
        let wide = windows::core::HSTRING::from(profile.as_str());
        let params = WLAN_CONNECTION_PARAMETERS {
            wlanConnectionMode: wlan_connection_mode_profile,
            strProfile: windows::core::PCWSTR(wide.as_ptr()),
            pDot11Ssid: std::ptr::null_mut(),
            pDesiredBssidList: std::ptr::null_mut(),
            dot11BssType: dot11_BSS_type_any,
            dwFlags: 0,
        };
        let r = WlanConnect(h, &g, &params, None);
        WlanCloseHandle(h, None);
        if r == 0 { Ok(profile) } else { Err(format!("connecting to {profile} failed ({r})")) }
    }
}

/// Calls `on_change` (from a Windows thread-pool thread) whenever an interface, an address or a route changes.
/// The caller debounces. Dropping the watcher stops the notifications.
pub struct Watcher {
    handles: Vec<HANDLE>,
    _ctx: Box<Box<dyn Fn() + Send + Sync>>,
}

unsafe extern "system" fn on_iface(ctx: *const core::ffi::c_void, _: *const MIB_IPINTERFACE_ROW, _: MIB_NOTIFICATION_TYPE) {
    (*(ctx as *const Box<dyn Fn() + Send + Sync>))();
}
unsafe extern "system" fn on_addr(ctx: *const core::ffi::c_void, _: *const MIB_UNICASTIPADDRESS_ROW, _: MIB_NOTIFICATION_TYPE) {
    (*(ctx as *const Box<dyn Fn() + Send + Sync>))();
}
unsafe extern "system" fn on_route(ctx: *const core::ffi::c_void, _: *const MIB_IPFORWARD_ROW2, _: MIB_NOTIFICATION_TYPE) {
    (*(ctx as *const Box<dyn Fn() + Send + Sync>))();
}

impl Watcher {
    pub fn start(on_change: impl Fn() + Send + Sync + 'static) -> Watcher {
        let ctx: Box<Box<dyn Fn() + Send + Sync>> = Box::new(Box::new(on_change));
        let c = &*ctx as *const Box<dyn Fn() + Send + Sync> as *const core::ffi::c_void;
        let mut handles = Vec::new();
        unsafe {
            let mut h = HANDLE::default();
            if NotifyIpInterfaceChange(AF_UNSPEC, Some(on_iface), Some(c), false, &mut h) == NO_ERROR {
                handles.push(h);
            }
            let mut h = HANDLE::default();
            if NotifyUnicastIpAddressChange(AF_UNSPEC, Some(on_addr), Some(c), false, &mut h) == NO_ERROR {
                handles.push(h);
            }
            let mut h = HANDLE::default();
            if NotifyRouteChange2(AF_UNSPEC, Some(on_route), c, false, &mut h) == NO_ERROR {
                handles.push(h);
            }
        }
        Watcher { handles, _ctx: ctx }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        // CancelMibChangeNotify2 waits for running callbacks, so the context outlives them.
        for h in self.handles.drain(..) {
            unsafe {
                let _ = CancelMibChangeNotify2(h);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iface(kind: Kind, guid: &str, status: Status, default_route: bool) -> Iface {
        Iface {
            index: 1,
            index6: 1,
            luid: 0,
            guid: guid.into(),
            alias: String::new(),
            desc: String::new(),
            kind,
            status,
            ipv4: vec![],
            nets: vec![],
            ipv6: vec![],
            gw4: None,
            gw6: None,
            metric4: 0,
            speed: 0,
            ssid: None,
            profile: None,
            default_route,
        }
    }

    #[test]
    fn status_needs_an_address_and_a_gateway() {
        let a = [Ipv4Addr::new(192, 168, 0, 34)];
        let gw = Some(Ipv4Addr::new(192, 168, 0, 1));
        assert_eq!(status_of(true, &a, &[], gw, None), Status::Connected);
        assert_eq!(status_of(true, &a, &[], None, None), Status::Limited);
        assert_eq!(status_of(true, &[Ipv4Addr::new(169, 254, 3, 4)], &[], gw, None), Status::Limited);
        assert_eq!(status_of(false, &a, &[], gw, None), Status::Disconnected);
        let v6 = ["2001:db8::5".parse().unwrap()];
        assert_eq!(status_of(true, &[], &v6, None, Some("fe80::1".parse().unwrap())), Status::Connected);
    }

    #[test]
    fn wifi_pick_prefers_the_setting_then_connected() {
        let list = vec![
            iface(Kind::Wifi, "{A}", Status::Disconnected, false),
            iface(Kind::Wifi, "{B}", Status::Connected, false),
            iface(Kind::Ethernet, "{E}", Status::Connected, true),
        ];
        assert_eq!(pick_wifi(&list, None).unwrap().guid, "{B}");
        assert_eq!(pick_wifi(&list, Some("{a}")).unwrap().guid, "{A}");
        assert_eq!(pick_wifi(&list, Some("{gone}")).unwrap().guid, "{B}");
        assert_eq!(pick_ethernet(&list).unwrap().guid, "{E}");
        assert!(pick_wifi(&list[2..], None).is_none());
    }

    #[test]
    fn guid_format_matches_adapter_name() {
        let g = GUID::from_u128(0x9b2c1a3e_1234_4abc_8def_0123456789ab);
        assert_eq!(guid_string(&g), "{9B2C1A3E-1234-4ABC-8DEF-0123456789AB}");
    }

    #[test]
    fn snapshot_reads_this_pc_without_changing_it() {
        // Read-only: lists adapters. It may be empty on a machine without physical adapters.
        for i in snapshot() {
            assert!(i.guid.starts_with('{'), "{:?}", i.guid);
        }
    }
}
