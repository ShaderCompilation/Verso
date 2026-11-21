//! What the responder says and when, without sockets or clocks of its own:
//! packets and timer ticks go in, packets to send come out. `run.rs` wires
//! it to the network. Section numbers refer to RFC 6762.
//!
//! It owns one name, `<label>.local`, with one A record (our IPv4 address)
//! and an NSEC record saying there is no other type, so dual-stack clients
//! don't wait for an IPv6 answer (§6.1). Lifecycle: probe that nobody else
//! uses the name (§8.1), pick the next name on a conflict (`verso-2`, …),
//! announce twice (§8.3), then answer queries. A new address starts over
//! with probing, since it may mean a different network.

use std::collections::VecDeque;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::{Duration, Instant};

use crate::dns::{
    CLASS_ANY, CLASS_IN, FLAG_AUTHORITATIVE, FLAG_RESPONSE, Message, Question, Record, TYPE_A, TYPE_AAAA, TYPE_ANY,
    TYPE_NSEC, encode_name,
};

pub const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
pub const PORT: u16 = 5353;
/// How long others may cache our records. Short, so a stale address after
/// a DHCP change can't linger long even if a goodbye was missed (§10 suggests
/// 120 s for records with the host name).
pub const TTL: u32 = 120;
/// Replies to simple resolvers that don't speak mDNS (§6.7).
const LEGACY_TTL: u32 = 10;
const PROBES: u8 = 3;
const PROBE_INTERVAL: Duration = Duration::from_millis(250);
const ANNOUNCEMENTS: u8 = 2;
const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(1);
/// Multicast the same answer at most this often (§6), or this often when
/// defending the name against someone else's probe.
const MULTICAST_GAP: Duration = Duration::from_secs(1);
const PROBE_DEFENSE_GAP: Duration = Duration::from_millis(250);
/// Unicast replies per second, at most: a bound on what anyone can make us send.
const UNICAST_PER_SECOND: u32 = 20;
/// After this many conflicts in `CONFLICT_WINDOW`, wait `CONFLICT_BACKOFF`
/// before each further probe (§8.1).
const CONFLICT_LIMIT: usize = 15;
const CONFLICT_WINDOW: Duration = Duration::from_secs(10);
const CONFLICT_BACKOFF: Duration = Duration::from_secs(5);
/// Losing a simultaneous probe tiebreak waits this long before probing again (§8.2).
const TIEBREAK_DELAY: Duration = Duration::from_secs(1);

/// The network interface we answer on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Link {
    pub addr: Ipv4Addr,
    pub netmask: Ipv4Addr,
    pub index: u32,
}

impl Link {
    /// Only devices on our own subnet may talk to us (§11): this, rather
    /// than replying only by multicast, stops off-link amplification.
    fn on_link(&self, ip: Ipv4Addr) -> bool {
        let mask = u32::from(self.netmask);
        u32::from(ip) & mask == u32::from(self.addr) & mask
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Send {
    pub to: SocketAddrV4,
    pub packet: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// No address, nothing to advertise.
    Idle,
    /// Checking nobody else uses the name. `sent` probes so far.
    Probing {
        sent: u8,
        next: Instant,
    },
    /// The name is ours; telling the network. Queries are answered too.
    Announcing {
        sent: u8,
        next: Instant,
    },
    Announced,
}

pub struct Responder {
    label: String,
    name: Vec<u8>,
    link: Option<Link>,
    phase: Phase,
    /// The last name we won, kept while probing again after a network change.
    won: Option<String>,
    conflicts: VecDeque<Instant>,
    last_multicast: Option<Instant>,
    unicast: (Instant, u32),
    random: u64,
}

impl Responder {
    /// `label` must pass [`crate::valid_label`]. `seed` varies the probe delay.
    pub fn new(label: &str, seed: u64, now: Instant) -> Self {
        Self {
            name: encode_name(&[label, "local"]),
            label: label.to_owned(),
            link: None,
            phase: Phase::Idle,
            won: None,
            conflicts: VecDeque::new(),
            last_multicast: None,
            unicast: (now, 0),
            random: seed | 1,
        }
    }

    /// The name being probed or announced.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The last name we won. It stays while probing again on a new network,
    /// and changes only if that probing finds the name taken there.
    pub fn won(&self) -> Option<&str> {
        self.won.as_deref()
    }

    pub fn link(&self) -> Option<Link> {
        self.link
    }

    /// Call when the interface or its address changes (or goes away).
    pub fn set_link(&mut self, link: Option<Link>, now: Instant) {
        if link == self.link {
            return;
        }
        self.link = link;
        self.phase = Phase::Idle;
        if link.is_some() {
            let delay = self.probe_delay();
            self.probe(now + delay);
        }
    }

    /// When `on_timer` next has work.
    pub fn deadline(&self) -> Option<Instant> {
        match self.phase {
            Phase::Probing { next, .. } | Phase::Announcing { next, .. } => Some(next),
            Phase::Idle | Phase::Announced => None,
        }
    }

    pub fn on_timer(&mut self, now: Instant) -> Vec<Send> {
        let mut out = Vec::new();
        loop {
            match self.phase {
                Phase::Probing { sent, next } if now >= next => {
                    if sent < PROBES {
                        out.push(self.multicast(self.probe_packet(sent == 0)));
                        self.phase = Phase::Probing { sent: sent + 1, next: now + PROBE_INTERVAL };
                    } else {
                        // Nobody objected within 250 ms of the last probe.
                        self.won = Some(self.label.clone());
                        self.phase = Phase::Announcing { sent: 0, next: now };
                    }
                }
                Phase::Announcing { sent, next } if now >= next => {
                    out.push(self.multicast(self.announcement(TTL)));
                    self.last_multicast = Some(now);
                    self.phase = if sent + 1 < ANNOUNCEMENTS {
                        Phase::Announcing { sent: sent + 1, next: now + ANNOUNCE_INTERVAL }
                    } else {
                        Phase::Announced
                    };
                }
                _ => return out,
            }
        }
    }

    /// A packet arrived from `src`. Returns the replies to send.
    pub fn on_packet(&mut self, src: SocketAddrV4, packet: &[u8], now: Instant) -> Vec<Send> {
        let Some(link) = self.link else { return Vec::new() };
        if !link.on_link(*src.ip()) {
            return Vec::new();
        }
        let Some(message) = Message::parse(packet) else { return Vec::new() };
        // Unknown operations and error replies are ignored (§18.3, §18.11).
        if message.opcode() != 0 || message.rcode() != 0 {
            return Vec::new();
        }
        if message.is_response() {
            // Real responses always come from port 5353 (§11).
            if src.port() == PORT {
                self.check_conflict(&message, now);
            }
            return Vec::new();
        }
        match self.phase {
            Phase::Idle => Vec::new(),
            Phase::Probing { .. } => {
                self.check_tiebreak(&message, now);
                Vec::new()
            }
            Phase::Announcing { .. } | Phase::Announced => self.answer(src, &message, now),
        }
    }

    /// Withdraws our records (TTL 0), so others forget them now rather than
    /// after the TTL (§10.1). Send before exiting.
    pub fn goodbye(&self) -> Vec<Send> {
        match self.phase {
            Phase::Announcing { .. } | Phase::Announced if self.link.is_some() => {
                vec![self.multicast(self.announcement(0))]
            }
            _ => Vec::new(),
        }
    }

    fn probe(&mut self, at: Instant) {
        self.phase = Phase::Probing { sent: 0, next: at };
    }

    /// A random 0–250 ms start, so devices powered on together don't probe in step (§8.1).
    fn probe_delay(&mut self) -> Duration {
        // xorshift64: plenty for spreading out a delay.
        self.random ^= self.random << 13;
        self.random ^= self.random >> 7;
        self.random ^= self.random << 17;
        Duration::from_millis(self.random % 250)
    }

    /// A response holds a record for our name that isn't ours.
    fn check_conflict(&mut self, message: &Message, now: Instant) {
        let ours = self.records(TTL, true);
        let conflict = message.records().any(|r| {
            // TTL 0 is someone withdrawing the name, which is no claim. A
            // conflict is a record of the same name and type as one of ours
            // with different data (§9): other types (a TXT or SRV for the
            // name) don't contradict our address. An identical record is no
            // conflict either: it could be our own packet looping back, or a
            // proxy repeating it.
            r.name == self.name
                && r.ttl > 0
                && ours.iter().any(|o| o.rtype == r.rtype)
                && !ours.iter().any(|o| o.rtype == r.rtype && o.data == r.data)
        });
        if !conflict {
            return;
        }
        match self.phase {
            Phase::Idle => {}
            Phase::Probing { .. } => {
                self.conflicts.push_back(now);
                while self.conflicts.front().is_some_and(|&t| now.duration_since(t) > CONFLICT_WINDOW) {
                    self.conflicts.pop_front();
                }
                self.label = crate::next_label(&self.label);
                self.name = encode_name(&[&self.label, "local"]);
                let delay = if self.conflicts.len() >= CONFLICT_LIMIT { CONFLICT_BACKOFF } else { self.probe_delay() };
                self.probe(now + delay);
            }
            // Someone else claims a name we announced: probe again, which
            // finds out who keeps it (§9).
            Phase::Announcing { .. } | Phase::Announced => self.probe(now),
        }
    }

    /// Someone probes for our name while we do (§8.2). Compare the records
    /// both want; the lexicographically later set wins, the other side waits
    /// a second and probes again.
    fn check_tiebreak(&mut self, message: &Message, now: Instant) {
        if !message.questions.iter().any(|q| q.name == self.name) {
            return;
        }
        let key = |r: &Record| (r.class, r.rtype, r.data.clone());
        let mut theirs: Vec<_> = message.authority.iter().filter(|r| r.name == self.name).map(key).collect();
        if theirs.is_empty() {
            return;
        }
        theirs.sort();
        let ours: Vec<_> = self.authority().iter().map(key).collect();
        // Equal is our own probe looping back.
        if theirs > ours {
            self.probe(now + TIEBREAK_DELAY);
        }
    }

    fn answer(&mut self, src: SocketAddrV4, query: &Message, now: Instant) -> Vec<Send> {
        // A query from a port other than 5353 comes from a simple resolver
        // that expects a classic DNS reply sent back to it (§6.7).
        let legacy = src.port() != PORT;
        let (mut want_a, mut want_nsec, mut unicast) = (false, false, false);
        let mut question = None;
        for q in &query.questions {
            if q.name != self.name || (q.class != CLASS_IN && q.class != CLASS_ANY) {
                continue;
            }
            match q.qtype {
                TYPE_A => want_a = true,
                TYPE_AAAA => want_nsec = true,
                TYPE_ANY => (want_a, want_nsec) = (true, true),
                _ => continue,
            }
            unicast |= q.unicast;
            question.get_or_insert(q);
        }
        let Some(question) = question else { return Vec::new() };
        // Known-answer suppression (§7.1): the asker already has our record
        // with at least half its lifetime left.
        if !legacy {
            let ours = self.records(TTL, true);
            let known = |rtype| {
                query.answers.iter().any(|r| {
                    r.name == self.name
                        && r.rtype == rtype
                        && r.ttl >= TTL / 2
                        && ours.iter().any(|o| o.rtype == rtype && o.data == r.data)
                })
            };
            want_a &= !known(TYPE_A);
            want_nsec &= !known(TYPE_NSEC);
            if !want_a && !want_nsec {
                return Vec::new();
            }
        }
        let [a, nsec] = self.records(if legacy { LEGACY_TTL } else { TTL }, !legacy);
        // What was asked goes in answers; the other record rides along as
        // additional, saving a second query.
        let (answers, additional) = match (want_a, want_nsec) {
            (true, true) => (vec![a, nsec], vec![]),
            (true, false) => (vec![a], vec![nsec]),
            _ => (vec![nsec], vec![a]),
        };
        let mut reply = Message {
            id: 0,
            flags: FLAG_RESPONSE | FLAG_AUTHORITATIVE,
            questions: vec![],
            answers,
            authority: vec![],
            additional,
        };
        if legacy {
            reply.id = query.id;
            reply.questions = vec![Question { unicast: false, ..question.clone() }];
        }
        if legacy || unicast {
            if now.duration_since(self.unicast.0) >= Duration::from_secs(1) {
                self.unicast = (now, 0);
            }
            if self.unicast.1 >= UNICAST_PER_SECOND {
                return Vec::new();
            }
            self.unicast.1 += 1;
            return vec![Send { to: src, packet: reply.encode() }];
        }
        // Someone probing for our name gets a quicker defense (§6).
        let gap = if query.authority.is_empty() { MULTICAST_GAP } else { PROBE_DEFENSE_GAP };
        if self.last_multicast.is_some_and(|t| now.duration_since(t) < gap) {
            return Vec::new();
        }
        self.last_multicast = Some(now);
        vec![self.multicast(reply)]
    }

    /// Our two records: the address, and the NSEC saying there's nothing else.
    fn records(&self, ttl: u32, cache_flush: bool) -> [Record; 2] {
        let addr = self.link.map_or(Ipv4Addr::UNSPECIFIED, |l| l.addr);
        // NSEC (§6.1): next name is our own; the type bitmap lists only A
        // (window 0, one byte, bit 1).
        let mut nsec = self.name.clone();
        nsec.extend([0, 1, 0x40]);
        let record = |rtype, data| Record { name: self.name.clone(), rtype, class: CLASS_IN, cache_flush, ttl, data };
        [record(TYPE_A, addr.octets().to_vec()), record(TYPE_NSEC, nsec)]
    }

    /// What a probe proposes: just the address record.
    fn authority(&self) -> Vec<Record> {
        let [a, _] = self.records(TTL, false);
        vec![a]
    }

    fn probe_packet(&self, unicast: bool) -> Message {
        Message {
            questions: vec![Question { name: self.name.clone(), qtype: TYPE_ANY, class: CLASS_IN, unicast }],
            authority: self.authority(),
            ..Message::default()
        }
    }

    fn announcement(&self, ttl: u32) -> Message {
        let [a, nsec] = self.records(ttl, true);
        Message {
            flags: FLAG_RESPONSE | FLAG_AUTHORITATIVE,
            answers: vec![a],
            additional: vec![nsec],
            ..Message::default()
        }
    }

    fn multicast(&self, message: Message) -> Send {
        Send { to: SocketAddrV4::new(GROUP, PORT), packet: message.encode() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: Ipv4Addr = Ipv4Addr::new(192, 168, 27, 10);
    const PEER: Ipv4Addr = Ipv4Addr::new(192, 168, 27, 20);

    fn link(addr: Ipv4Addr) -> Link {
        Link { addr, netmask: Ipv4Addr::new(255, 255, 255, 0), index: 3 }
    }

    /// A responder that has won `verso` and finished announcing.
    fn announced(now: &mut Instant) -> Responder {
        let mut r = Responder::new("verso", 42, *now);
        r.set_link(Some(link(ME)), *now);
        for _ in 0..12 {
            *now += Duration::from_millis(250);
            r.on_timer(*now);
        }
        assert_eq!(r.phase, Phase::Announced);
        *now += Duration::from_secs(2);
        r
    }

    fn query(name: &[&str], qtype: u16, unicast: bool) -> Message {
        Message {
            id: 0x4242,
            questions: vec![Question { name: encode_name(name), qtype, class: CLASS_IN, unicast }],
            ..Message::default()
        }
    }

    fn a_record(addr: Ipv4Addr, ttl: u32) -> Record {
        Record {
            name: encode_name(&["verso", "local"]),
            rtype: TYPE_A,
            class: CLASS_IN,
            cache_flush: true,
            ttl,
            data: addr.octets().to_vec(),
        }
    }

    fn from(ip: Ipv4Addr, port: u16) -> SocketAddrV4 {
        SocketAddrV4::new(ip, port)
    }

    fn parse(send: &Send) -> Message {
        Message::parse(&send.packet).unwrap()
    }

    #[test]
    fn probes_three_times_then_announces_twice() {
        let mut now = Instant::now();
        let mut r = Responder::new("verso", 1, now);
        assert!(r.on_timer(now).is_empty());
        r.set_link(Some(link(ME)), now);
        let mut sent = Vec::new();
        for _ in 0..20 {
            now += Duration::from_millis(125);
            sent.extend(r.on_timer(now).into_iter().map(|s| parse(&s)));
        }
        let probes: Vec<_> = sent.iter().filter(|m| !m.is_response()).collect();
        let announcements: Vec<_> = sent.iter().filter(|m| m.is_response()).collect();
        assert_eq!(probes.len(), 3);
        assert!(probes[0].questions[0].unicast && !probes[1].questions[0].unicast);
        assert_eq!(probes[0].questions[0].qtype, TYPE_ANY);
        assert_eq!(probes[0].authority[0].data, ME.octets());
        assert_eq!(announcements.len(), 2);
        assert_eq!(announcements[0].answers, vec![a_record(ME, TTL)]);
        assert_eq!(announcements[0].additional[0].rtype, TYPE_NSEC);
        assert_eq!(r.won(), Some("verso"));
    }

    #[test]
    fn answers_a_queries_by_multicast_with_rate_limit() {
        let mut now = Instant::now();
        let mut r = announced(&mut now);
        let q = query(&["VERSO", "local"], TYPE_A, false).encode();
        let replies = r.on_packet(from(PEER, PORT), &q, now);
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].to, SocketAddrV4::new(GROUP, PORT));
        let reply = parse(&replies[0]);
        assert_eq!(reply.answers, vec![a_record(ME, TTL)]);
        assert_eq!(reply.additional[0].rtype, TYPE_NSEC);
        // Again within a second: suppressed.
        assert!(r.on_packet(from(PEER, PORT), &q, now + Duration::from_millis(500)).is_empty());
        assert_eq!(r.on_packet(from(PEER, PORT), &q, now + Duration::from_millis(1100)).len(), 1);
    }

    #[test]
    fn ignores_other_names_types_and_off_link_senders() {
        let mut now = Instant::now();
        let mut r = announced(&mut now);
        let other = query(&["printer", "local"], TYPE_A, false).encode();
        assert!(r.on_packet(from(PEER, PORT), &other, now).is_empty());
        let txt = query(&["verso", "local"], 16, false).encode();
        assert!(r.on_packet(from(PEER, PORT), &txt, now).is_empty());
        let ours = query(&["verso", "local"], TYPE_A, false).encode();
        assert!(r.on_packet(from(Ipv4Addr::new(10, 0, 0, 5), PORT), &ours, now).is_empty());
        assert!(r.on_packet(from(PEER, PORT), b"\x00\x01garbage", now).is_empty());
    }

    #[test]
    fn answers_aaaa_with_nsec() {
        let mut now = Instant::now();
        let mut r = announced(&mut now);
        let q = query(&["verso", "local"], TYPE_AAAA, false).encode();
        let reply = parse(&r.on_packet(from(PEER, PORT), &q, now)[0]);
        assert_eq!(reply.answers.len(), 1);
        let nsec = &reply.answers[0];
        assert_eq!(nsec.rtype, TYPE_NSEC);
        assert_eq!(nsec.data, b"\x05verso\x05local\x00\x00\x01\x40");
        assert_eq!(reply.additional, vec![a_record(ME, TTL)]);
    }

    #[test]
    fn unicast_and_legacy_replies() {
        let mut now = Instant::now();
        let mut r = announced(&mut now);
        // QU bit: straight back to the asker, normal TTL and cache-flush.
        let q = query(&["verso", "local"], TYPE_A, true).encode();
        let replies = r.on_packet(from(PEER, PORT), &q, now);
        assert_eq!(replies[0].to, from(PEER, PORT));
        assert_eq!(parse(&replies[0]).answers, vec![a_record(ME, TTL)]);
        // Legacy: from another port; echo ID and question, short TTL, no cache-flush.
        let replies = r.on_packet(from(PEER, 40000), &q, now);
        assert_eq!(replies[0].to, from(PEER, 40000));
        let reply = parse(&replies[0]);
        assert_eq!(reply.id, 0x4242);
        assert_eq!(reply.questions.len(), 1);
        assert!(!reply.questions[0].unicast);
        assert_eq!(reply.answers, vec![Record { cache_flush: false, ..a_record(ME, LEGACY_TTL) }]);
    }

    #[test]
    fn unicast_replies_are_bounded() {
        let mut now = Instant::now();
        let mut r = announced(&mut now);
        let q = query(&["verso", "local"], TYPE_A, true).encode();
        let sent: usize = (0..100).map(|_| r.on_packet(from(PEER, 40000), &q, now).len()).sum();
        assert_eq!(sent, UNICAST_PER_SECOND as usize);
        assert_eq!(r.on_packet(from(PEER, 40000), &q, now + Duration::from_secs(1)).len(), 1);
    }

    #[test]
    fn known_answers_suppress_replies() {
        let mut now = Instant::now();
        let mut r = announced(&mut now);
        let mut q = query(&["verso", "local"], TYPE_A, false);
        q.answers = vec![a_record(ME, TTL)];
        assert!(r.on_packet(from(PEER, PORT), &q.encode(), now).is_empty());
        // Nearly expired: answer, so the asker refreshes.
        q.answers = vec![a_record(ME, 30)];
        assert_eq!(r.on_packet(from(PEER, PORT), &q.encode(), now).len(), 1);
    }

    #[test]
    fn renames_on_conflict_while_probing() {
        let mut now = Instant::now();
        let mut r = Responder::new("verso", 7, now);
        r.set_link(Some(link(ME)), now);
        now += Duration::from_millis(300);
        r.on_timer(now);
        let other = Message {
            flags: FLAG_RESPONSE | FLAG_AUTHORITATIVE,
            answers: vec![a_record(PEER, TTL)],
            ..Message::default()
        };
        r.on_packet(from(PEER, PORT), &other.encode(), now);
        assert_eq!(r.label(), "verso-2");
        for _ in 0..12 {
            now += Duration::from_millis(250);
            r.on_timer(now);
        }
        assert_eq!(r.won(), Some("verso-2"));
        // Responses from a port other than 5353 aren't real mDNS answers.
        let mut r = Responder::new("verso", 7, now);
        r.set_link(Some(link(ME)), now);
        r.on_packet(from(PEER, 40000), &other.encode(), now);
        assert_eq!(r.label(), "verso");
    }

    #[test]
    fn records_of_other_types_are_no_conflict() {
        let mut now = Instant::now();
        let mut r = announced(&mut now);
        let txt = Message {
            flags: FLAG_RESPONSE | FLAG_AUTHORITATIVE,
            answers: vec![Record { rtype: 16, data: vec![0], ..a_record(PEER, TTL) }],
            ..Message::default()
        };
        r.on_packet(from(PEER, PORT), &txt.encode(), now);
        assert_eq!(r.phase, Phase::Announced);
        assert_eq!(r.label(), "verso");
        // A different address for the name still is one.
        let other = Message {
            flags: FLAG_RESPONSE | FLAG_AUTHORITATIVE,
            answers: vec![a_record(PEER, TTL)],
            ..Message::default()
        };
        r.on_packet(from(PEER, PORT), &other.encode(), now);
        assert!(matches!(r.phase, Phase::Probing { .. }));
    }

    #[test]
    fn own_packets_and_goodbyes_are_no_conflict() {
        let mut now = Instant::now();
        let mut r = Responder::new("verso", 7, now);
        r.set_link(Some(link(ME)), now);
        let mut looped = Vec::new();
        for _ in 0..12 {
            now += Duration::from_millis(250);
            for send in r.on_timer(now) {
                looped.push(send.packet);
            }
            // Multicast loopback hands our own packets back to us.
            for packet in &looped {
                r.on_packet(from(ME, PORT), packet, now);
            }
            looped.clear();
        }
        assert_eq!(r.won(), Some("verso"));
        assert_eq!(r.phase, Phase::Announced);
        let goodbye = Message {
            flags: FLAG_RESPONSE | FLAG_AUTHORITATIVE,
            answers: vec![a_record(PEER, 0)],
            ..Message::default()
        };
        r.on_packet(from(PEER, PORT), &goodbye.encode(), now);
        assert_eq!(r.phase, Phase::Announced);
    }

    #[test]
    fn reprobes_when_an_announced_name_is_claimed() {
        let mut now = Instant::now();
        let mut r = announced(&mut now);
        let other = Message {
            flags: FLAG_RESPONSE | FLAG_AUTHORITATIVE,
            answers: vec![a_record(PEER, TTL)],
            ..Message::default()
        };
        r.on_packet(from(PEER, PORT), &other.encode(), now);
        assert!(matches!(r.phase, Phase::Probing { sent: 0, .. }));
        assert_eq!(r.label(), "verso");
        assert_eq!(r.won(), Some("verso"));
    }

    #[test]
    fn simultaneous_probe_tiebreak() {
        let probe_from = |addr: Ipv4Addr| Message {
            questions: vec![Question {
                name: encode_name(&["verso", "local"]),
                qtype: TYPE_ANY,
                class: CLASS_IN,
                unicast: false,
            }],
            authority: vec![Record { cache_flush: false, ..a_record(addr, TTL) }],
            ..Message::default()
        };
        // A higher address wins: we wait a second.
        let now = Instant::now();
        let mut r = Responder::new("verso", 7, now);
        r.set_link(Some(link(ME)), now);
        r.on_packet(from(PEER, PORT), &probe_from(PEER).encode(), now);
        assert_eq!(r.phase, Phase::Probing { sent: 0, next: now + TIEBREAK_DELAY });
        // A lower address loses: we carry on.
        let mut r = Responder::new("verso", 7, now);
        r.set_link(Some(link(ME)), now);
        let before = r.phase;
        let lower = Ipv4Addr::new(192, 168, 27, 2);
        r.on_packet(from(lower, PORT), &probe_from(lower).encode(), now);
        assert_eq!(r.phase, before);
    }

    #[test]
    fn defends_the_name_against_probes() {
        let mut now = Instant::now();
        let mut r = announced(&mut now);
        let mut probe = query(&["verso", "local"], TYPE_ANY, false);
        probe.authority = vec![Record { cache_flush: false, ..a_record(PEER, TTL) }];
        let first = r.on_packet(from(PEER, PORT), &probe.encode(), now);
        assert_eq!(parse(&first[0]).answers.len(), 2); // ANY: A and NSEC
        let second = r.on_packet(from(PEER, PORT), &probe.encode(), now + Duration::from_millis(260));
        assert_eq!(second.len(), 1);
    }

    #[test]
    fn new_address_probes_again_and_goodbye_withdraws() {
        let mut now = Instant::now();
        let mut r = announced(&mut now);
        let goodbye = r.goodbye();
        assert_eq!(parse(&goodbye[0]).answers, vec![a_record(ME, 0)]);
        let new = Ipv4Addr::new(192, 168, 27, 11);
        r.set_link(Some(link(new)), now);
        assert!(matches!(r.phase, Phase::Probing { .. }));
        assert!(r.goodbye().is_empty());
        let mut sent = Vec::new();
        for _ in 0..12 {
            now += Duration::from_millis(250);
            sent.extend(r.on_timer(now));
        }
        assert!(sent.iter().any(|s| parse(s).answers == vec![a_record(new, TTL)]));
        r.set_link(None, now);
        assert_eq!(r.phase, Phase::Idle);
        assert_eq!(r.deadline(), None);
        assert!(r.on_packet(from(PEER, PORT), &query(&["verso", "local"], TYPE_A, false).encode(), now).is_empty());
    }
}
