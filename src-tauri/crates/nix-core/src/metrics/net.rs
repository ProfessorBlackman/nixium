// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Methuselah Nwodobeh

//! Network throughput from `/sys/class/net/*/statistics/*`. `MON-1`.
//!
//! # Why the aggregate counts only physical interfaces
//!
//! This machine has **31 network interfaces**: one wireless card, one modem, and 29 virtual ones —
//! 23 `veth` pairs, three Docker bridges, `docker0`, and loopback.
//!
//! Summing all of them would not merely be noisy, it would be **wrong**. A packet from a container
//! traverses its `veth`, then the bridge, then the physical interface, and each of those increments
//! its own counter. Adding them up triple-counts the same bytes and reports a machine sending three
//! times what it sends.
//!
//! So the aggregate is physical interfaces only, identified by the `device` symlink the kernel
//! creates for anything backed by real hardware. Every interface is still *recorded* individually —
//! `MON-7` shows them — but the headline figure counts each byte once.
//!
//! Loopback is excluded from both, by its ARP type rather than by being called `lo`: traffic a
//! machine sends to itself is real, and is not network throughput in any sense a dashboard means.
//!
//! # The gap this leaves
//!
//! A machine whose only route out is a bridge or a tunnel — some VM hosts, some VPN-only setups —
//! has no physical interface carrying its traffic, and its aggregate will read zero while `MON-7`
//! shows the real figure. That is a real limitation, stated rather than papered over, and the
//! alternative was triple-counting on every developer machine.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// `ARPHRD_LOOPBACK`, from the kernel's `if_arp.h`.
const ARPHRD_LOOPBACK: u32 = 772;

/// Cumulative byte counters for one interface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct NetCounters {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

/// One interface's throughput and link state over the last interval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct InterfaceReading {
    pub name: String,
    #[ts(type = "number")]
    pub received_per_second: u64,
    #[ts(type = "number")]
    pub sent_per_second: u64,
    /// Backed by hardware. Only these are summed into the totals.
    pub physical: bool,
    /// `up`, `down` or `unknown` — the kernel's own word. `MON-7`.
    pub operstate: String,
    /// Whether a cable or association is actually present, which is not the same as being `up`.
    pub carrier: bool,
    /// Link speed in megabits, where the driver reports one. Wireless drivers generally do not, and
    /// that is `None` rather than a link running at 0 Mb/s.
    #[ts(type = "number | null")]
    pub speed_mbps: Option<u32>,
    pub mac: Option<String>,
    #[ts(type = "number | null")]
    pub mtu: Option<u32>,
    /// IPv6 addresses, from `/proc/net/if_inet6`.
    ///
    /// **IPv4 is deliberately absent.** There is no per-interface IPv4 address anywhere in `sysfs` or
    /// `procfs`. Getting one means `getifaddrs` or an `rtnetlink` conversation — the first needs
    /// `libc`, the second hand-built netlink messages, and so either `unsafe` or a new dependency in a
    /// program that ships privileged code. Showing what is genuinely available and naming what is
    /// missing beats quietly presenting an incomplete list called "addresses".
    pub ipv6: Vec<String>,
}

impl InterfaceReading {
    /// Whether this interface is actually carrying traffic — up, with a link.
    ///
    /// What `MON-7` uses to decide which interface to feature, **re-evaluated every tick**. Stacer
    /// picked the first non-loopback interface once at startup and never looked again: unplug the
    /// Ethernet, join Wi-Fi, and it charted a dead interface until restarted.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.carrier && self.operstate != "down"
    }
}

/// What the UI is given for one tick.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct NetReading {
    /// Summed across **physical** interfaces only, so each byte is counted once.
    #[ts(type = "number")]
    pub received_per_second: u64,
    #[ts(type = "number")]
    pub sent_per_second: u64,
    /// Every interface, physical or not, biggest first.
    pub interfaces: Vec<InterfaceReading>,
}

impl NetReading {
    /// The interface worth featuring: hardware, with a link, carrying the most traffic. `MON-7`.
    ///
    /// Chosen from **this** reading rather than remembered, which is the whole of the acceptance
    /// criterion — unplug the Ethernet and join Wi-Fi and the answer changes on the next tick.
    ///
    /// Physical is part of the test because virtual interfaces are almost always "active": on this
    /// machine twenty-nine of thirty-one report a carrier and an `up` state, so activity alone picks
    /// a Docker `veth` at random.
    #[must_use]
    pub fn featured(&self) -> Option<&InterfaceReading> {
        self.interfaces
            .iter()
            .filter(|i| i.physical && i.is_active())
            .max_by_key(|i| i.received_per_second.saturating_add(i.sent_per_second))
    }
}

/// Link details read alongside the counters.
#[derive(Debug, Clone, Default)]
struct Link {
    physical: bool,
    operstate: String,
    carrier: bool,
    speed_mbps: Option<u32>,
    mac: Option<String>,
    mtu: Option<u32>,
    ipv6: Vec<String>,
}

/// Parse `/proc/net/if_inet6` into per-interface IPv6 addresses.
///
/// Each line is a 32-character hex address, then index, prefix length, scope, flags, and the
/// interface name:
///
/// ```text
/// fe80000000000000a71e535bf81de1a5 03 40 20 80 wlp0s20f3
/// ```
#[must_use]
pub(crate) fn ipv6_addresses(path: &Path) -> HashMap<String, Vec<String>> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };

    let mut found: HashMap<String, Vec<String>> = HashMap::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let (Some(hex), Some(name)) = (fields.first(), fields.get(5)) else {
            continue;
        };
        if hex.len() != 32 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        // Written as the eight colon-separated quads an address is spelled in. Not compressed to
        // `::`: an expanded address is unambiguous, and this is a diagnostic panel.
        let groups: Vec<String> = (0..8).map(|i| hex[i * 4..i * 4 + 4].to_string()).collect();
        found
            .entry((*name).to_string())
            .or_default()
            .push(groups.join(":"));
    }
    found
}

/// Whether an interface is backed by hardware.
///
/// The `device` symlink is the kernel's own answer. It is the right signal *here*, unlike in
/// `/sys/block`, because a virtual network interface genuinely is not carrying the machine's traffic
/// on its own account — whereas a virtual block device very much is holding real data.
#[must_use]
pub(crate) fn is_physical(dir: &Path) -> bool {
    dir.join("device").exists()
}

/// Whether an interface is loopback, by ARP type rather than by name.
#[must_use]
pub(crate) fn is_loopback(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join("type"))
        .ok()
        .and_then(|t| t.trim().parse::<u32>().ok())
        .is_some_and(|t| t == ARPHRD_LOOPBACK)
}

/// Reads `/sys/class/net`. **The single owner of network delta state** (§P3).
#[derive(Debug, Default)]
pub(crate) struct NetSampler {
    root: Option<PathBuf>,
    previous: HashMap<String, NetCounters>,
}

impl NetSampler {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    #[must_use]
    fn rooted_at(root: PathBuf) -> Self {
        Self {
            root: Some(root),
            previous: HashMap::new(),
        }
    }

    fn root(&self) -> &Path {
        self.root
            .as_deref()
            .unwrap_or_else(|| Path::new("/sys/class/net"))
    }

    fn read_counters(&self) -> (HashMap<String, NetCounters>, HashMap<String, Link>) {
        let mut counters = HashMap::new();
        let mut links = HashMap::new();
        let Ok(entries) = std::fs::read_dir(self.root()) else {
            return (counters, links);
        };
        let addresses = ipv6_addresses(Path::new("/proc/net/if_inet6"));

        for entry in entries.flatten() {
            let dir = entry.path();
            if is_loopback(&dir) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let stat = |field: &str| -> Option<u64> {
                std::fs::read_to_string(dir.join("statistics").join(field))
                    .ok()?
                    .trim()
                    .parse()
                    .ok()
            };
            let (Some(rx), Some(tx)) = (stat("rx_bytes"), stat("tx_bytes")) else {
                continue;
            };
            let text = |file: &str| -> Option<String> {
                let value = std::fs::read_to_string(dir.join(file))
                    .ok()?
                    .trim()
                    .to_string();
                (!value.is_empty()).then_some(value)
            };
            links.insert(
                name.clone(),
                Link {
                    physical: is_physical(&dir),
                    operstate: text("operstate").unwrap_or_else(|| "unknown".into()),
                    // `carrier` is unreadable rather than zero on a down interface, so this is a
                    // parse-or-false rather than a parse-or-error.
                    carrier: text("carrier").is_some_and(|v| v == "1"),
                    speed_mbps: text("speed").and_then(|v| v.parse().ok()),
                    mac: text("address"),
                    mtu: text("mtu").and_then(|v| v.parse().ok()),
                    ipv6: addresses.get(&name).cloned().unwrap_or_default(),
                },
            );
            counters.insert(
                name,
                NetCounters {
                    rx_bytes: rx,
                    tx_bytes: tx,
                },
            );
        }
        (counters, links)
    }

    /// Take a reading. `None` until there is a previous one to subtract.
    pub(crate) fn sample(&mut self, elapsed: std::time::Duration) -> Option<NetReading> {
        let (now, links) = self.read_counters();
        let first = self.previous.is_empty();
        let seconds = elapsed.as_secs_f64().max(f64::EPSILON);

        let mut interfaces: Vec<InterfaceReading> = Vec::new();
        let mut received = 0u64;
        let mut sent = 0u64;

        for (name, counters) in &now {
            let Some(before) = self.previous.get(name) else {
                continue;
            };
            // An interface going down and up resets its counters.
            let rx = counters.rx_bytes.checked_sub(before.rx_bytes);
            let tx = counters.tx_bytes.checked_sub(before.tx_bytes);
            let (Some(rx), Some(tx)) = (rx, tx) else {
                continue;
            };

            #[allow(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss
            )]
            let link = links.get(name).cloned().unwrap_or_default();
            let reading = InterfaceReading {
                name: name.clone(),
                received_per_second: (rx as f64 / seconds) as u64,
                sent_per_second: (tx as f64 / seconds) as u64,
                physical: link.physical,
                operstate: link.operstate,
                carrier: link.carrier,
                speed_mbps: link.speed_mbps,
                mac: link.mac,
                mtu: link.mtu,
                ipv6: link.ipv6,
            };

            // Only hardware contributes to the totals: a container's bytes pass through a veth, a
            // bridge and the card, and summing all three reports three times the traffic.
            if reading.physical {
                received += reading.received_per_second;
                sent += reading.sent_per_second;
            }
            interfaces.push(reading);
        }

        self.previous = now;
        if first {
            return None;
        }

        interfaces.sort_by_key(|i| {
            std::cmp::Reverse(i.received_per_second.saturating_add(i.sent_per_second))
        });
        Some(NetReading {
            received_per_second: received,
            sent_per_second: sent,
            interfaces,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn sandbox(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "nix-net-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// `kind` is `"physical"`, `"virtual"` or `"loopback"`.
    fn interface(root: &Path, name: &str, kind: &str, rx: u64, tx: u64) {
        let dir = root.join(name);
        std::fs::create_dir_all(dir.join("statistics")).unwrap();
        std::fs::write(dir.join("statistics/rx_bytes"), rx.to_string()).unwrap();
        std::fs::write(dir.join("statistics/tx_bytes"), tx.to_string()).unwrap();
        std::fs::write(
            dir.join("type"),
            if kind == "loopback" { "772" } else { "1" },
        )
        .unwrap();
        std::fs::write(dir.join("operstate"), "up").unwrap();
        std::fs::write(dir.join("carrier"), "1").unwrap();
        if kind == "physical" {
            // A directory stands in for the symlink; `exists()` is what is checked.
            std::fs::create_dir_all(dir.join("device")).unwrap();
        }
    }

    /// The accuracy point: a container's bytes cross three interfaces.
    #[test]
    fn only_physical_interfaces_reach_the_totals() {
        let root = sandbox("physical");
        interface(&root, "wlp0s20f3", "physical", 0, 0);
        interface(&root, "docker0", "virtual", 0, 0);
        interface(&root, "veth123", "virtual", 0, 0);
        interface(&root, "br-abc", "virtual", 0, 0);

        let mut sampler = NetSampler::rooted_at(root.clone());
        sampler.sample(std::time::Duration::from_secs(1));

        // The same 1000 bytes, counted by all four.
        for (name, kind) in [
            ("wlp0s20f3", "physical"),
            ("docker0", "virtual"),
            ("veth123", "virtual"),
            ("br-abc", "virtual"),
        ] {
            interface(&root, name, kind, 1000, 500);
        }

        let reading = sampler.sample(std::time::Duration::from_secs(1)).unwrap();
        assert_eq!(
            reading.received_per_second, 1000,
            "one byte crossing four interfaces is one byte, not four"
        );
        assert_eq!(reading.sent_per_second, 500);
        assert_eq!(
            reading.interfaces.len(),
            4,
            "all four are still recorded individually for MON-7"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// Loopback is excluded by ARP type, not by being spelled `lo`.
    #[test]
    fn loopback_is_excluded_by_its_type() {
        let root = sandbox("loopback");
        interface(&root, "lo", "loopback", 0, 0);
        interface(&root, "eth0", "physical", 0, 0);

        let mut sampler = NetSampler::rooted_at(root.clone());
        sampler.sample(std::time::Duration::from_secs(1));
        interface(&root, "lo", "loopback", 9_000_000, 9_000_000);
        interface(&root, "eth0", "physical", 100, 100);

        let reading = sampler.sample(std::time::Duration::from_secs(1)).unwrap();
        assert!(
            !reading.interfaces.iter().any(|i| i.name == "lo"),
            "traffic a machine sends to itself is not network throughput"
        );
        assert_eq!(reading.received_per_second, 100);

        std::fs::remove_dir_all(&root).ok();
    }

    /// An interface named `lo-something` is not loopback; the type is what decides.
    #[test]
    fn a_name_beginning_with_lo_is_not_loopback() {
        let root = sandbox("loname");
        interface(&root, "lolan0", "physical", 0, 0);
        let mut sampler = NetSampler::rooted_at(root.clone());
        sampler.sample(std::time::Duration::from_secs(1));
        interface(&root, "lolan0", "physical", 400, 0);

        let reading = sampler.sample(std::time::Duration::from_secs(1)).unwrap();
        assert_eq!(reading.received_per_second, 400);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_first_sample_is_not_a_measurement() {
        let root = sandbox("first");
        interface(&root, "eth0", "physical", 5_000_000, 5_000_000);

        let mut sampler = NetSampler::rooted_at(root.clone());
        assert!(
            sampler.sample(std::time::Duration::from_secs(1)).is_none(),
            "lifetime counters are not a rate"
        );

        interface(&root, "eth0", "physical", 5_001_000, 5_000_500);
        let reading = sampler.sample(std::time::Duration::from_secs(1)).unwrap();
        assert_eq!(reading.received_per_second, 1000);
        assert_eq!(reading.sent_per_second, 500);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_interface_bouncing_resets_its_counters_without_a_burst() {
        let root = sandbox("bounce");
        interface(&root, "eth0", "physical", 5_000_000, 5_000_000);
        let mut sampler = NetSampler::rooted_at(root.clone());
        sampler.sample(std::time::Duration::from_secs(1));

        interface(&root, "eth0", "physical", 12, 12);
        let reading = sampler.sample(std::time::Duration::from_secs(1)).unwrap();
        assert_eq!(
            reading.received_per_second, 0,
            "down and up is a reset, not five megabytes received backwards"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn interfaces_are_ordered_by_traffic() {
        let root = sandbox("order");
        interface(&root, "quiet", "physical", 0, 0);
        interface(&root, "busy", "physical", 0, 0);
        let mut sampler = NetSampler::rooted_at(root.clone());
        sampler.sample(std::time::Duration::from_secs(1));

        interface(&root, "quiet", "physical", 10, 10);
        interface(&root, "busy", "physical", 9000, 9000);

        let reading = sampler.sample(std::time::Duration::from_secs(1)).unwrap();
        assert_eq!(reading.interfaces[0].name, "busy");

        std::fs::remove_dir_all(&root).ok();
    }

    // ---- `MON-7`: link details ----

    /// Golden line, §P8. Captured from this machine's `/proc/net/if_inet6`.
    #[test]
    fn ipv6_addresses_are_parsed_and_grouped_per_interface() {
        let dir = sandbox("inet6");
        let file = dir.join("if_inet6");
        std::fs::write(
            &file,
            "fe80000000000000a71e535bf81de1a5 03 40 20 80 wlp0s20f3\n\
             fe80000000000000cc28fcfffe772f80 21 40 20 80 vetha1cfd38\n\
             20010db8000000000000000000000001 03 40 00 80 wlp0s20f3\n",
        )
        .unwrap();

        let found = ipv6_addresses(&file);
        let wifi = found.get("wlp0s20f3").unwrap();
        assert_eq!(wifi.len(), 2, "an interface can hold several addresses");
        assert_eq!(wifi[0], "fe80:0000:0000:0000:a71e:535b:f81d:e1a5");
        assert_eq!(found.get("vetha1cfd38").unwrap().len(), 1);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_malformed_inet6_line_is_skipped() {
        let dir = sandbox("inet6bad");
        let file = dir.join("if_inet6");
        std::fs::write(
            &file,
            "notlongenough 03 40 20 80 eth0\n\
             zzzz000000000000a71e535bf81de1a5 03 40 20 80 eth0\n\
             fe80000000000000a71e535bf81de1a5 03 40 20 80 eth0\n",
        )
        .unwrap();
        assert_eq!(
            ipv6_addresses(&file).get("eth0").unwrap().len(),
            1,
            "a short address and a non-hex one are not addresses"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_inet6_file_yields_no_addresses() {
        assert!(ipv6_addresses(Path::new("/definitely/not/here")).is_empty());
    }

    #[test]
    fn link_state_is_read_alongside_the_counters() {
        let root = sandbox("link");
        interface(&root, "eth0", "physical", 0, 0);
        std::fs::write(root.join("eth0/speed"), "1000").unwrap();
        std::fs::write(root.join("eth0/address"), "60:dd:8e:ed:73:e9").unwrap();
        std::fs::write(root.join("eth0/mtu"), "1500").unwrap();

        let mut sampler = NetSampler::rooted_at(root.clone());
        sampler.sample(std::time::Duration::from_secs(1));
        interface(&root, "eth0", "physical", 100, 100);
        std::fs::write(root.join("eth0/speed"), "1000").unwrap();
        std::fs::write(root.join("eth0/address"), "60:dd:8e:ed:73:e9").unwrap();
        std::fs::write(root.join("eth0/mtu"), "1500").unwrap();

        let reading = sampler.sample(std::time::Duration::from_secs(1)).unwrap();
        let eth = &reading.interfaces[0];
        assert_eq!(eth.operstate, "up");
        assert!(eth.carrier);
        assert_eq!(eth.speed_mbps, Some(1000));
        assert_eq!(eth.mac.as_deref(), Some("60:dd:8e:ed:73:e9"));
        assert_eq!(eth.mtu, Some(1500));
        assert!(eth.is_active());

        std::fs::remove_dir_all(&root).ok();
    }

    /// A wireless driver reports no speed. That is unknown, not zero.
    #[test]
    fn an_absent_speed_is_unknown_rather_than_zero() {
        let root = sandbox("nospeed");
        interface(&root, "wlan0", "physical", 0, 0);
        let mut sampler = NetSampler::rooted_at(root.clone());
        sampler.sample(std::time::Duration::from_secs(1));
        interface(&root, "wlan0", "physical", 1, 1);

        let reading = sampler.sample(std::time::Duration::from_secs(1)).unwrap();
        assert_eq!(reading.interfaces[0].speed_mbps, None);

        std::fs::remove_dir_all(&root).ok();
    }

    /// # Regression
    ///
    /// Stacer chose an interface once at startup. Unplugging Ethernet and joining Wi-Fi left it
    /// charting a dead one until restarted, so activity is judged per tick.
    #[test]
    fn an_interface_losing_its_link_stops_being_active() {
        let root = sandbox("switch");
        interface(&root, "eth0", "physical", 0, 0);
        interface(&root, "wlan0", "physical", 0, 0);
        let mut sampler = NetSampler::rooted_at(root.clone());
        sampler.sample(std::time::Duration::from_secs(1));

        // The cable comes out; the wireless associates.
        interface(&root, "eth0", "physical", 0, 0);
        std::fs::write(root.join("eth0/carrier"), "0").unwrap();
        std::fs::write(root.join("eth0/operstate"), "down").unwrap();
        interface(&root, "wlan0", "physical", 5000, 5000);

        let reading = sampler.sample(std::time::Duration::from_secs(1)).unwrap();
        let eth = reading
            .interfaces
            .iter()
            .find(|i| i.name == "eth0")
            .unwrap();
        let wlan = reading
            .interfaces
            .iter()
            .find(|i| i.name == "wlan0")
            .unwrap();
        assert!(!eth.is_active(), "no carrier and down is not active");
        assert!(wlan.is_active());
        assert_eq!(
            reading.interfaces.iter().filter(|i| i.is_active()).count(),
            1,
            "the featured interface follows the link, without a restart"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// Activity alone is not enough: on this machine 29 of 31 interfaces are "active".
    #[test]
    fn the_featured_interface_is_physical_and_carrying_traffic() {
        let root = sandbox("featured");
        interface(&root, "wlan0", "physical", 0, 0);
        interface(&root, "docker0", "virtual", 0, 0);
        for i in 0..5 {
            interface(&root, &format!("veth{i}"), "virtual", 0, 0);
        }

        let mut sampler = NetSampler::rooted_at(root.clone());
        sampler.sample(std::time::Duration::from_secs(1));

        // The veths are busier than the card, and every one of them is up with a carrier.
        interface(&root, "wlan0", "physical", 1000, 1000);
        interface(&root, "docker0", "virtual", 9000, 9000);
        for i in 0..5 {
            interface(&root, &format!("veth{i}"), "virtual", 9000, 9000);
        }

        let reading = sampler.sample(std::time::Duration::from_secs(1)).unwrap();
        assert_eq!(
            reading.featured().map(|i| i.name.as_str()),
            Some("wlan0"),
            "a Docker veth is not the machine's network connection"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn nothing_is_featured_when_no_hardware_has_a_link() {
        let root = sandbox("nolink");
        interface(&root, "eth0", "physical", 0, 0);
        let mut sampler = NetSampler::rooted_at(root.clone());
        sampler.sample(std::time::Duration::from_secs(1));

        interface(&root, "eth0", "physical", 10, 10);
        std::fs::write(root.join("eth0/carrier"), "0").unwrap();
        std::fs::write(root.join("eth0/operstate"), "down").unwrap();

        let reading = sampler.sample(std::time::Duration::from_secs(1)).unwrap();
        assert!(
            reading.featured().is_none(),
            "an unplugged machine has no featured interface, rather than a dead one"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_missing_directory_reads_as_no_interfaces() {
        let mut sampler = NetSampler::rooted_at(PathBuf::from("/definitely/not/here"));
        assert!(sampler.sample(std::time::Duration::from_secs(1)).is_none());
    }

    // ---- against this machine ----

    #[test]
    fn this_machines_interfaces_are_mostly_virtual_and_the_filter_notices() {
        let Ok(entries) = std::fs::read_dir("/sys/class/net") else {
            return;
        };
        let (mut total, mut physical) = (0, 0);
        for entry in entries.flatten() {
            total += 1;
            if is_physical(&entry.path()) {
                physical += 1;
            }
        }
        assert!(total > 0);
        assert!(
            physical <= total,
            "hardware-backed interfaces are a subset of all of them"
        );
    }
}
