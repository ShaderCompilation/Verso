//! The small part of DNS a one-name mDNS responder needs (RFC 1035, RFC 6762).
//!
//! Incoming packets come from anyone on the network, so parsing is strict and
//! bounded: names follow compression pointers only backwards (no loops), are
//! at most 255 bytes, and anything malformed is rejected whole. Names are kept
//! in uncompressed wire format, lowercased, so comparing them is comparing
//! bytes (mDNS names match case-insensitively). We never compress our own
//! names: they are short, and it keeps the encoder trivial.

pub const TYPE_A: u16 = 1;
pub const TYPE_AAAA: u16 = 28;
pub const TYPE_NSEC: u16 = 47;
pub const TYPE_ANY: u16 = 255;
pub const CLASS_IN: u16 = 1;
pub const CLASS_ANY: u16 = 255;

/// The top bit of a question's class asks for a unicast reply (§5.4); of a
/// record's class, it tells caches to drop other records for the name (§10.2).
const CLASS_TOP_BIT: u16 = 0x8000;
pub const FLAG_RESPONSE: u16 = 0x8000;
pub const FLAG_AUTHORITATIVE: u16 = 0x0400;
const MAX_NAME: usize = 255;
/// Questions plus records in one message. Real mDNS traffic stays far below.
const MAX_ENTRIES: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    /// Uncompressed wire format, lowercased.
    pub name: Vec<u8>,
    pub qtype: u16,
    /// Without the unicast-response bit.
    pub class: u16,
    /// The asker wants the answer sent to it directly.
    pub unicast: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// Uncompressed wire format, lowercased.
    pub name: Vec<u8>,
    pub rtype: u16,
    /// Without the cache-flush bit.
    pub class: u16,
    pub cache_flush: bool,
    pub ttl: u32,
    /// Raw; any names inside may be compressed relative to the packet.
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Message {
    pub id: u16,
    pub flags: u16,
    pub questions: Vec<Question>,
    pub answers: Vec<Record>,
    pub authority: Vec<Record>,
    pub additional: Vec<Record>,
}

impl Message {
    pub fn is_response(&self) -> bool {
        self.flags & FLAG_RESPONSE != 0
    }

    pub fn opcode(&self) -> u16 {
        (self.flags >> 11) & 0xF
    }

    pub fn rcode(&self) -> u16 {
        self.flags & 0xF
    }

    /// Every record in the message, whatever its section.
    pub fn records(&self) -> impl Iterator<Item = &Record> {
        self.answers.iter().chain(&self.authority).chain(&self.additional)
    }

    pub fn parse(packet: &[u8]) -> Option<Message> {
        let mut r = Reader { packet, pos: 0 };
        let id = r.u16()?;
        let flags = r.u16()?;
        let counts = [r.u16()?, r.u16()?, r.u16()?, r.u16()?];
        if counts.iter().map(|&c| usize::from(c)).sum::<usize>() > MAX_ENTRIES {
            return None;
        }
        let mut message = Message { id, flags, ..Message::default() };
        for _ in 0..counts[0] {
            let name = r.name()?;
            let qtype = r.u16()?;
            let class = r.u16()?;
            message.questions.push(Question {
                name,
                qtype,
                class: class & !CLASS_TOP_BIT,
                unicast: class & CLASS_TOP_BIT != 0,
            });
        }
        for (section, &count) in counts[1..].iter().enumerate() {
            for _ in 0..count {
                let record = r.record()?;
                match section {
                    0 => message.answers.push(record),
                    1 => message.authority.push(record),
                    _ => message.additional.push(record),
                }
            }
        }
        Some(message)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(128);
        for value in [
            self.id,
            self.flags,
            self.questions.len() as u16,
            self.answers.len() as u16,
            self.authority.len() as u16,
            self.additional.len() as u16,
        ] {
            out.extend(value.to_be_bytes());
        }
        for q in &self.questions {
            out.extend(&q.name);
            out.extend(q.qtype.to_be_bytes());
            out.extend((q.class | if q.unicast { CLASS_TOP_BIT } else { 0 }).to_be_bytes());
        }
        for record in self.records() {
            out.extend(&record.name);
            out.extend(record.rtype.to_be_bytes());
            out.extend((record.class | if record.cache_flush { CLASS_TOP_BIT } else { 0 }).to_be_bytes());
            out.extend(record.ttl.to_be_bytes());
            out.extend((record.data.len() as u16).to_be_bytes());
            out.extend(&record.data);
        }
        out
    }
}

/// `labels` in uncompressed wire format, lowercased: `["verso", "local"]` →
/// `\x05verso\x05local\x00`.
pub fn encode_name(labels: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    for label in labels {
        out.push(label.len() as u8);
        out.extend(label.bytes().map(|b| b.to_ascii_lowercase()));
    }
    out.push(0);
    out
}

struct Reader<'a> {
    packet: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn bytes(&mut self, n: usize) -> Option<&[u8]> {
        let bytes = self.packet.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(bytes)
    }

    fn u16(&mut self) -> Option<u16> {
        self.bytes(2).map(|b| u16::from_be_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Option<u32> {
        self.bytes(4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn record(&mut self) -> Option<Record> {
        let name = self.name()?;
        let rtype = self.u16()?;
        let class = self.u16()?;
        let ttl = self.u32()?;
        let len = usize::from(self.u16()?);
        let data = self.bytes(len)?.to_vec();
        Some(Record { name, rtype, class: class & !CLASS_TOP_BIT, cache_flush: class & CLASS_TOP_BIT != 0, ttl, data })
    }

    /// Reads a possibly compressed name and leaves `pos` after it.
    fn name(&mut self) -> Option<Vec<u8>> {
        let mut name = Vec::new();
        let mut pos = self.pos;
        // Where reading continues once the name is done: after the first pointer.
        let mut resume = None;
        // Each pointer must point before the stretch being read, so following
        // them always moves backwards and ends.
        let mut limit = pos;
        loop {
            let len = usize::from(*self.packet.get(pos)?);
            match len & 0xC0 {
                0x00 if len == 0 => {
                    name.push(0);
                    self.pos = resume.unwrap_or(pos + 1);
                    return Some(name);
                }
                0x00 => {
                    let label = self.packet.get(pos + 1..pos + 1 + len)?;
                    // The label, its length byte and the final zero must fit.
                    if name.len() + len + 2 > MAX_NAME {
                        return None;
                    }
                    name.push(len as u8);
                    name.extend(label.iter().map(u8::to_ascii_lowercase));
                    pos += 1 + len;
                }
                0xC0 => {
                    let target = ((len & 0x3F) << 8) | usize::from(*self.packet.get(pos + 1)?);
                    if target >= limit {
                        return None;
                    }
                    resume.get_or_insert(pos + 2);
                    limit = target;
                    pos = target;
                }
                _ => return None, // 0x40 and 0x80 label types are obsolete.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(qd: u16, an: u16) -> Vec<u8> {
        let mut p = vec![0x12, 0x34, 0, 0];
        for c in [qd, an, 0, 0] {
            p.extend(c.to_be_bytes());
        }
        p
    }

    #[test]
    fn round_trips() {
        let message = Message {
            id: 7,
            flags: FLAG_RESPONSE | FLAG_AUTHORITATIVE,
            questions: vec![Question {
                name: encode_name(&["verso", "local"]),
                qtype: TYPE_A,
                class: CLASS_IN,
                unicast: true,
            }],
            answers: vec![Record {
                name: encode_name(&["verso", "local"]),
                rtype: TYPE_A,
                class: CLASS_IN,
                cache_flush: true,
                ttl: 120,
                data: vec![192, 168, 1, 2],
            }],
            authority: vec![],
            additional: vec![Record {
                name: encode_name(&["x", "local"]),
                rtype: TYPE_NSEC,
                class: CLASS_IN,
                cache_flush: false,
                ttl: 0,
                data: vec![1, 2, 3],
            }],
        };
        assert_eq!(Message::parse(&message.encode()), Some(message));
    }

    #[test]
    fn lowercases_and_follows_pointers() {
        // Two questions; the second reuses "local" from the first.
        let mut p = header(2, 0);
        p.extend(b"\x05VeRsO\x05local\x00\x00\x01\x80\x01"); // A, QU
        p.extend(b"\x05other\xc0\x12\x00\x1c\x00\x01"); // pointer to "local" at 18
        let m = Message::parse(&p).unwrap();
        assert_eq!(m.questions[0].name, encode_name(&["verso", "local"]));
        assert!(m.questions[0].unicast);
        assert_eq!(m.questions[0].class, CLASS_IN);
        assert_eq!(m.questions[1].name, encode_name(&["other", "local"]));
        assert_eq!(m.questions[1].qtype, TYPE_AAAA);
        assert!(!m.questions[1].unicast);
    }

    #[test]
    fn rejects_pointer_loops_and_forward_pointers() {
        // A pointer to itself.
        let mut p = header(1, 0);
        p.extend(b"\xc0\x0c\x00\x01\x00\x01");
        assert_eq!(Message::parse(&p), None);
        // A label followed by a pointer back to that label: a loop.
        let mut p = header(1, 0);
        p.extend(b"\x01a\xc0\x0c\x00\x01\x00\x01");
        assert_eq!(Message::parse(&p), None);
        // A pointer forwards, past the end.
        let mut p = header(1, 0);
        p.extend(b"\xc0\xff\x00\x01\x00\x01");
        assert_eq!(Message::parse(&p), None);
    }

    #[test]
    fn rejects_truncated_and_oversized_input() {
        let mut p = header(1, 0);
        p.extend(b"\x05verso\x05local\x00\x00\x01\x00\x01");
        for end in 0..p.len() {
            assert_eq!(Message::parse(&p[..end]), None, "truncated at {end}");
        }
        assert!(Message::parse(&p).is_some());
        // A record whose data runs past the end.
        let mut p = header(0, 1);
        p.extend(b"\x00\x00\x01\x00\x01\x00\x00\x00\x78\x00\x04\x01\x02");
        assert_eq!(Message::parse(&p), None);
        // Names over 255 bytes: each question adds a 63-byte label in front
        // of the previous question's name. The fourth reaches 257 bytes.
        let questions = |n: u16| {
            let mut p = header(n, 0);
            let mut previous = None;
            for _ in 0..n {
                let here = p.len();
                p.push(63);
                p.extend([b'a'; 63]);
                match previous {
                    None => p.push(0),
                    Some(at) => p.extend((0xC000 | at as u16).to_be_bytes()),
                }
                p.extend(b"\x00\x01\x00\x01");
                previous = Some(here);
            }
            p
        };
        assert!(Message::parse(&questions(3)).is_some());
        assert_eq!(Message::parse(&questions(4)), None);
        // Counts no packet of this size could hold.
        let mut p = header(300, 0);
        p.extend([0; 32]);
        assert_eq!(Message::parse(&p), None);
    }

    #[test]
    fn rejects_obsolete_label_types() {
        let mut p = header(1, 0);
        p.extend(b"\x45\x00\x00\x01\x00\x01");
        assert_eq!(Message::parse(&p), None);
    }
}
