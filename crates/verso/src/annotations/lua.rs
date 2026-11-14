//! Bounded Lua *data* parser: no names, calls, expressions or execution.
use anyhow::{Result, bail, ensure};
use std::collections::BTreeMap;
#[derive(Clone, Debug, serde::Serialize)]
pub enum Value {
    String(String),
    Table(BTreeMap<String, Value>),
    Scalar(String),
}
impl Value {
    pub fn table(&self) -> Result<&BTreeMap<String, Value>> {
        if let Self::Table(t) = self { Ok(t) } else { bail!("expected a Lua table") }
    }
    pub fn string(&self) -> Option<&str> {
        if let Self::String(s) = self { Some(s) } else { None }
    }
}
struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
    nodes: usize,
    cancelled: &'a dyn Fn() -> bool,
}
#[cfg(test)]
pub fn parse(bytes: &[u8]) -> Result<Value> {
    parse_cancellable(bytes, &|| false)
}
pub fn parse_cancellable(bytes: &[u8], cancelled: &dyn Fn() -> bool) -> Result<Value> {
    ensure!(bytes.len() <= 16 * 1024 * 1024, "KOReader metadata exceeds 16 MiB");
    let mut p = Parser { bytes, at: 0, nodes: 0, cancelled };
    p.space()?;
    ensure!(p.take(b"return"), "expected serialized Lua return table");
    let value = p.value(0)?;
    p.space()?;
    ensure!(p.at == bytes.len(), "unsupported Lua code after data table");
    Ok(value)
}
impl Parser<'_> {
    fn take(&mut self, s: &[u8]) -> bool {
        if self.bytes[self.at..].starts_with(s) {
            self.at += s.len();
            true
        } else {
            false
        }
    }
    fn long(&mut self) -> Result<Option<Vec<u8>>> {
        let begin = self.at;
        if !self.take(b"[") {
            return Ok(None);
        }
        let mut equals = 0;
        while self.take(b"=") {
            equals += 1;
        }
        if !self.take(b"[") {
            self.at = begin;
            return Ok(None);
        }
        let mut end = vec![b']'];
        end.extend(vec![b'='; equals]);
        end.push(b']');
        let _ = self.take(b"\r\n") || self.take(b"\n"); // Lua drops an initial newline.
        let start = self.at;
        let offset = self.bytes[start..]
            .windows(end.len())
            .position(|w| w == end)
            .ok_or_else(|| anyhow::anyhow!("unterminated long string"))?;
        self.at += offset + end.len();
        Ok(Some(self.bytes[start..start + offset].to_vec()))
    }
    fn space(&mut self) -> Result<()> {
        loop {
            while self.bytes.get(self.at).is_some_and(u8::is_ascii_whitespace) {
                self.at += 1;
            }
            if !self.take(b"--") {
                break;
            }
            if self.long()?.is_none() {
                while self.bytes.get(self.at).is_some_and(|b| *b != b'\n') {
                    self.at += 1;
                }
            }
        }
        Ok(())
    }
    fn value(&mut self, depth: usize) -> Result<Value> {
        self.nodes += 1;
        if self.nodes.is_multiple_of(1024) {
            ensure!(!(self.cancelled)(), "import cancelled");
        }
        ensure!(depth < 64 && self.nodes < 500_000, "Lua data complexity limit");
        self.space()?;
        if let Some(s) = self.long()? {
            return Ok(Value::String(String::from_utf8(s)?));
        }
        if self.take(b"{") {
            let mut table = BTreeMap::new();
            let mut index = 1;
            loop {
                self.space()?;
                if self.take(b"}") {
                    break;
                }
                let key;
                let value;
                if self.take(b"[") {
                    self.space()?;
                    key = match self.value(depth + 1)? {
                        Value::String(s) => s,
                        Value::Scalar(s) => {
                            ensure!(s.parse::<usize>().is_ok(), "unsupported Lua key");
                            s
                        }
                        _ => bail!("unsupported Lua key"),
                    };
                    self.space()?;
                    ensure!(self.take(b"]"), "expected ]");
                    self.space()?;
                    ensure!(self.take(b"="), "expected =");
                    value = self.value(depth + 1)?;
                } else {
                    let start = self.at;
                    while self.bytes.get(self.at).is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_') {
                        self.at += 1;
                    }
                    let name = String::from_utf8(self.bytes[start..self.at].to_vec())?;
                    self.space()?;
                    if !name.is_empty() && self.take(b"=") {
                        key = name;
                        value = self.value(depth + 1)?;
                    } else {
                        self.at = start;
                        key = index.to_string();
                        index += 1;
                        value = self.value(depth + 1)?;
                    }
                }
                ensure!(!table.contains_key(&key), "duplicate Lua table key");
                table.insert(key, value);
                self.space()?;
                if self.take(b"}") {
                    break;
                }
                ensure!(self.take(b",") || self.take(b";"), "expected table separator");
            }
            return Ok(Value::Table(table));
        }
        if let Some(&quote @ (b'\'' | b'"')) = self.bytes.get(self.at) {
            self.at += 1;
            let mut out = Vec::new();
            loop {
                let c = *self.bytes.get(self.at).ok_or_else(|| anyhow::anyhow!("unterminated Lua string"))?;
                self.at += 1;
                if c == quote {
                    break;
                }
                if c != b'\\' {
                    out.push(c);
                    continue;
                }
                let c = *self.bytes.get(self.at).ok_or_else(|| anyhow::anyhow!("incomplete escape"))?;
                self.at += 1;
                match c {
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'a' => out.push(7),
                    b'b' => out.push(8),
                    b'f' => out.push(12),
                    b'v' => out.push(11),
                    b'\\' | b'\'' | b'"' => out.push(c),
                    b'\n' => out.push(b'\n'),
                    b'z' => {
                        while self.bytes.get(self.at).is_some_and(u8::is_ascii_whitespace) {
                            self.at += 1;
                        }
                    }
                    b'x' => {
                        let end = self.at + 2;
                        ensure!(end <= self.bytes.len(), "incomplete hex escape");
                        out.push(u8::from_str_radix(std::str::from_utf8(&self.bytes[self.at..end])?, 16)?);
                        self.at = end;
                    }
                    b'0'..=b'9' => {
                        let mut n = (c - b'0') as u16;
                        for _ in 0..2 {
                            if let Some(b @ b'0'..=b'9') = self.bytes.get(self.at) {
                                n = n * 10 + (*b - b'0') as u16;
                                self.at += 1;
                            } else {
                                break;
                            }
                        }
                        ensure!(n <= 255, "invalid decimal escape");
                        out.push(n as u8);
                    }
                    _ => bail!("unsupported Lua escape"),
                }
            }
            return Ok(Value::String(String::from_utf8(out)?));
        }
        let start = self.at;
        while self.bytes.get(self.at).is_some_and(|b| !b.is_ascii_whitespace() && !b",;]}".contains(b)) {
            self.at += 1;
        }
        let token = std::str::from_utf8(&self.bytes[start..self.at])?;
        ensure!(
            matches!(token, "true" | "false" | "nil") || token.parse::<f64>().is_ok_and(f64::is_finite),
            "unsupported Lua expression: {token}"
        );
        Ok(Value::Scalar(token.to_owned()))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn data_only_unicode_and_escapes() {
        let v = parse(br#"-- comment
return { ["annotations"] = { [1] = {text="\208\159\209\128\208\184\208\178\208\181\209\130\n\"hi\"", note=[=[hello]=]}, }, ["other"]=false }"#).unwrap();
        assert_eq!(
            v.table().unwrap()["annotations"].table().unwrap()["1"].table().unwrap()["text"].string(),
            Some("Привет\n\"hi\"")
        );
        for code in ["return os.execute('x')", "return {}; evil()", "return {x=function() end}", "return {x=1+2}"] {
            assert!(parse(code.as_bytes()).is_err());
        }
    }
}
