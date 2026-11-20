//! Opening a TCP port in the Kindle's firewall, for the SSH server
//! (platform/ssh.rs) and Wi-Fi transfer (transfer/). KOReader's exact rules:
//! INPUT for new and established connections to the port, OUTPUT for the
//! replies. The launcher removes port 80's the same way after a crash.

use std::process::{Command, Stdio};

/// Adds both rules (checking first, so they never pile up), or removes
/// every copy of each.
pub fn set(port: u16, open: bool) {
    let rules = [
        format!("INPUT -p tcp --dport {port} -m conntrack --ctstate NEW,ESTABLISHED -j ACCEPT"),
        format!("OUTPUT -p tcp --sport {port} -m conntrack --ctstate ESTABLISHED -j ACCEPT"),
    ];
    let iptables = |op: &str, rule: &str| {
        Command::new("iptables")
            .arg(op)
            .args(rule.split_whitespace())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    for rule in &rules {
        if open {
            if !iptables("-C", rule) && !iptables("-A", rule) {
                log::warn!("couldn't open port {port} in the firewall");
            }
        } else {
            // Remove every copy (another tool may have added one too).
            for _ in 0..4 {
                if !iptables("-D", rule) {
                    break;
                }
            }
        }
    }
}
