//! How this computer is woken from a phone on the same network (Wake-on-LAN): each wired or wireless card's MAC address, the broadcast address
//! of its IPv4 network and this computer's address there. It goes with the registration; the phone sends the magic packet itself (rova-backend
//! devices/wake.ts). A virtual, loopback or link-local card is left out.

use std::net::{IpAddr, Ipv4Addr};

use serde_json::{json, Value};

pub fn wake_targets() -> Value {
    let networks = sysinfo::Networks::new_with_refreshed_list();
    let mut targets = Vec::new();
    for (name, data) in &networks {
        let lowered = name.to_lowercase();
        if ["lo", "utun", "awdl", "llw", "bridge", "vmnet", "vbox", "docker", "veth", "tailscale", "zerotier", "wg", "anpi", "ap1", "gif", "stf"]
            .iter()
            .any(|prefix| lowered.starts_with(prefix))
            || lowered.contains("virtual")
            || lowered.contains("loopback")
            || lowered.contains("vpn")
        {
            continue;
        }
        let mac = data.mac_address().to_string().to_lowercase().replace('-', ":");
        if mac == "00:00:00:00:00:00" {
            continue;
        }
        let Some((address, prefix)) = data.ip_networks().iter().find_map(|network| match network.addr {
            IpAddr::V4(address) if !address.is_loopback() && !address.is_link_local() => Some((address, network.prefix)),
            _ => None,
        }) else {
            continue;
        };
        targets.push(json!({ "mac": mac, "address": address.to_string(), "broadcast": broadcast(address, prefix).to_string() }));
        if targets.len() >= 4 {
            break;
        }
    }
    Value::Array(targets)
}

fn broadcast(address: Ipv4Addr, prefix: u8) -> Ipv4Addr {
    let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - u32::from(prefix.min(32))) };
    Ipv4Addr::from(u32::from(address) | !mask)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_broadcast_address_is_the_network_with_every_host_bit_set() {
        assert_eq!(broadcast(Ipv4Addr::new(192, 168, 1, 23), 24), Ipv4Addr::new(192, 168, 1, 255));
        assert_eq!(broadcast(Ipv4Addr::new(10, 0, 5, 7), 16), Ipv4Addr::new(10, 0, 255, 255));
    }
}
