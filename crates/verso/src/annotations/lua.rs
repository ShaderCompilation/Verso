//! Bounded Lua *data* parser: no names, calls, expressions or execution.
use anyhow::{Context, Result, bail, ensure};
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
/// Which values to build. Skipped values are still checked as data, with the
/// same limits, but nothing is allocated for them.
#[derive(Clone, Copy)]
enum Keep<'a> {
    All,
    Nothing,
    /// The table's entries with these keys, each built in full.
    Only(&'a [&'a str]),
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
    document(bytes, Keep::All, cancelled)
}
#[cfg_attr(not(test), allow(dead_code))] // The KOReader progress reader (crate::koreader).
/// The `wanted` top-level fields of a `return { … }` table, each built in full.
/// Every other value is checked and skipped without being built, so a sidecar
/// with thousands of highlights costs little more than a small one. Missing
/// fields are simply absent. Same limits as `parse_cancellable`.
pub fn fields_cancellable(
    bytes: &[u8],
    wanted: &[&str],
    cancelled: &dyn Fn() -> bool,
) -> Result<BTreeMap<String, Value>> {
    match document(bytes, Keep::Only(wanted), cancelled)? {
        Value::Table(t) => Ok(t),
        _ => bail!("expected a Lua table"),
    }
}
fn document(bytes: &[u8], keep: Keep, cancelled: &dyn Fn() -> bool) -> Result<Value> {
    ensure!(bytes.len() <= 16 * 1024 * 1024, "KOReader metadata exceeds 16 MiB");
    let mut p = Parser { bytes, at: 0, nodes: 0, cancelled };
    p.space()?;
    ensure!(p.take(b"return"), "expected serialized Lua return table");
    let value = p.value(0, keep)?.context("expected a Lua value")?;
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
    /// A long bracket string's byte range, or None when there is none here.
    fn long(&mut self) -> Result<Option<std::ops::Range<usize>>> {
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
        Ok(Some(start..start + offset))
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
    /// The next value, or None when `keep` is `Nothing` and it was skipped.
    fn value(&mut self, depth: usize, keep: Keep) -> Result<Option<Value>> {
        let build = !matches!(keep, Keep::Nothing);
        self.nodes += 1;
        if self.nodes.is_multiple_of(1024) {
            ensure!(!(self.cancelled)(), "import cancelled");
        }
        ensure!(depth < 64 && self.nodes < 500_000, "Lua data complexity limit");
        self.space()?;
        if let Some(range) = self.long()? {
            if !build {
                return Ok(None);
            }
            return Ok(Some(Value::String(String::from_utf8(self.bytes[range].to_vec())?)));
        }
        if self.take(b"{") {
            let mut table = BTreeMap::new();
            let mut index = 1;
            loop {
                self.space()?;
                if self.take(b"}") {
                    break;
                }
                // Keys are built only for a table that is itself built.
                let key;
                if self.take(b"[") {
                    self.space()?;
                    // A skipped key isn't built, but it can't be a table either.
                    ensure!(build || self.bytes.get(self.at) != Some(&b'{'), "unsupported Lua key");
                    key = match self.value(depth + 1, if build { Keep::All } else { Keep::Nothing })? {
                        None => None,
                        Some(Value::String(s)) => Some(s),
                        Some(Value::Scalar(s)) => {
                            ensure!(s.parse::<usize>().is_ok(), "unsupported Lua key");
                            Some(s)
                        }
                        Some(_) => bail!("unsupported Lua key"),
                    };
                    self.space()?;
                    ensure!(self.take(b"]"), "expected ]");
                    self.space()?;
                    ensure!(self.take(b"="), "expected =");
                } else {
                    let start = self.at;
                    while self.bytes.get(self.at).is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_') {
                        self.at += 1;
                    }
                    let end = self.at;
                    self.space()?;
                    if end > start && self.take(b"=") {
                        key = build.then(|| String::from_utf8(self.bytes[start..end].to_vec())).transpose()?;
                    } else {
                        self.at = start;
                        key = build.then(|| index.to_string());
                        index += 1;
                    }
                }
                let child = match keep {
                    Keep::Only(wanted) if !key.as_deref().is_some_and(|k| wanted.contains(&k)) => Keep::Nothing,
                    Keep::Only(_) => Keep::All,
                    other => other,
                };
                if let (Some(key), Some(value)) = (key, self.value(depth + 1, child)?) {
                    ensure!(!table.contains_key(&key), "duplicate Lua table key");
                    table.insert(key, value);
                }
                self.space()?;
                if self.take(b"}") {
                    break;
                }
                ensure!(self.take(b",") || self.take(b";"), "expected table separator");
            }
            return Ok(build.then_some(Value::Table(table)));
        }
        if let Some(&quote @ (b'\'' | b'"')) = self.bytes.get(self.at) {
            self.at += 1;
            let mut out = Vec::new();
            let mut push = |c| {
                if build {
                    out.push(c);
                }
            };
            loop {
                let c = *self.bytes.get(self.at).ok_or_else(|| anyhow::anyhow!("unterminated Lua string"))?;
                self.at += 1;
                if c == quote {
                    break;
                }
                if c != b'\\' {
                    push(c);
                    continue;
                }
                let c = *self.bytes.get(self.at).ok_or_else(|| anyhow::anyhow!("incomplete escape"))?;
                self.at += 1;
                match c {
                    b'n' => push(b'\n'),
                    b'r' => push(b'\r'),
                    b't' => push(b'\t'),
                    b'a' => push(7),
                    b'b' => push(8),
                    b'f' => push(12),
                    b'v' => push(11),
                    b'\\' | b'\'' | b'"' => push(c),
                    b'\n' => push(b'\n'),
                    b'z' => {
                        while self.bytes.get(self.at).is_some_and(u8::is_ascii_whitespace) {
                            self.at += 1;
                        }
                    }
                    b'x' => {
                        let end = self.at + 2;
                        ensure!(end <= self.bytes.len(), "incomplete hex escape");
                        push(u8::from_str_radix(std::str::from_utf8(&self.bytes[self.at..end])?, 16)?);
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
                        push(n as u8);
                    }
                    _ => bail!("unsupported Lua escape"),
                }
            }
            if !build {
                return Ok(None);
            }
            return Ok(Some(Value::String(String::from_utf8(out)?)));
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
        Ok(build.then(|| Value::Scalar(token.to_owned())))
    }
}
#[cfg(test)]
pub(crate) mod tests {
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
    /// A KOReader sidecar as KOReader writes it: keys sorted, so the
    /// annotations come before the position fields and `stats` after them.
    pub fn sidecar(annotations: usize) -> String {
        let mut s = String::from("-- we can read Lua syntax here!\nreturn {\n    [\"annotations\"] = {\n");
        for i in 1..=annotations {
            s.push_str(&format!(
                "        [{i}] = {{\n            [\"chapter\"] = \"Глава {i}\",\n            [\"datetime\"] = \"2026-11-20 08:00:00\",\n            [\"drawer\"] = \"lighten\",\n            [\"note\"] = [[A note\nwith \\\" and ]],\n            [\"pos0\"] = \"/body/DocFragment[{i}]/body/p[3]/text().0\",\n            [\"pos1\"] = \"/body/DocFragment[{i}]/body/p[3]/text().42\",\n            [\"text\"] = \"Quote \\208\\159 \\\"{i}\\\"\",\n        }},\n"
            ));
        }
        s.push_str(concat!(
            "    },\n",
            "    [\"cre_dom_version\"] = 20240114,\n",
            "    [\"doc_props\"] = { [\"title\"] = \"Book\", [\"authors\"] = \"A\\nB\" },\n",
            "    [\"last_xpointer\"] = \"/body/DocFragment[12]/body/p[5]/text().17\",\n",
            "    [\"percent_finished\"] = 0.6215,\n",
            "    [\"stats\"] = { [\"highlights\"] = 3, [\"pages\"] = 412, [\"performance_in_pages\"] = { [1700000000] = 12 } },\n",
            "    [\"summary\"] = {\n        [\"modified\"] = \"2026-11-20\",\n        [\"status\"] = \"reading\",\n    },\n",
            "}\n",
        ));
        s
    }
    const WANTED: &[&str] = &["last_xpointer", "percent_finished", "summary"];
    fn fields(text: &str) -> Result<BTreeMap<String, Value>> {
        fields_cancellable(text.as_bytes(), WANTED, &|| false)
    }
    #[test]
    fn fields_of_modern_and_legacy_sidecars() -> Result<()> {
        let modern = fields(&sidecar(3))?;
        assert_eq!(modern.keys().collect::<Vec<_>>(), WANTED, "nothing else is built");
        assert_eq!(modern["last_xpointer"].string(), Some("/body/DocFragment[12]/body/p[5]/text().17"));
        assert!(matches!(&modern["percent_finished"], Value::Scalar(s) if s == "0.6215"));
        assert_eq!(modern["summary"].table()?["status"].string(), Some("reading"), "wanted tables are built in full");
        // The legacy format: `highlight` and `bookmarks` instead of
        // `annotations`, bare keys, no summary.
        let legacy = fields(
            "return { bookmarks = { { page = '/body/p[2]', notes = 'x' } }, highlight = { [12] = { { text = 'Hi', pos0 = '/a' } } }, last_xpointer = '/body/p[9]', percent_finished = 0.25 }",
        )?;
        assert_eq!(legacy.keys().collect::<Vec<_>>(), ["last_xpointer", "percent_finished"]);
        assert_eq!(legacy["last_xpointer"].string(), Some("/body/p[9]"));
        Ok(())
    }
    #[test]
    fn missing_fields_and_nested_tables_around_the_wanted_ones() -> Result<()> {
        assert!(fields("return {}")?.is_empty());
        assert!(fields("return { doc_props = { title = 'x' } }")?.is_empty());
        // A nested key with a wanted name is not a top-level field.
        let nested = fields(
            "return { a = { last_xpointer = '/inner', b = { { summary = 1 } } }, percent_finished = 1, z = { { last_xpointer = '/inner2' } } }",
        )?;
        assert_eq!(nested.keys().collect::<Vec<_>>(), ["percent_finished"]);
        // Skipped values are still checked: code anywhere is refused.
        for code in [
            "return { a = os.execute('x'), last_xpointer = '/p' }",
            "return { last_xpointer = '/p', z = { function() end } }",
            "return { a = { [{}] = 1 } , last_xpointer = '/p' }",
            "return { a = 'unterminated, last_xpointer = '/p' }",
            "return { last_xpointer = '/p' } evil()",
            "return { last_xpointer = '/p', last_xpointer = '/q' }",
        ] {
            assert!(fields(code).is_err(), "{code}");
        }
        assert!(fields("return 'not a table'").is_err());
        Ok(())
    }
    #[test]
    fn large_sidecars_match_the_full_parser_and_stay_bounded() -> Result<()> {
        let text = sidecar(5000);
        let full = parse(text.as_bytes())?;
        let chosen = fields(&text)?;
        assert_eq!(full.table()?["annotations"].table()?.len(), 5000);
        assert_eq!(
            serde_json::to_value(&chosen)?,
            serde_json::to_value(
                full.table()?.iter().filter(|(k, _)| WANTED.contains(&k.as_str())).collect::<BTreeMap<_, _>>()
            )?
        );
        // The size limit applies before anything is read.
        let mut huge = b"return { s = '".to_vec();
        huge.resize(16 * 1024 * 1024 + 1, b'x');
        assert!(fields_cancellable(&huge, WANTED, &|| false).unwrap_err().to_string().contains("16 MiB"));
        // Cancelling stops a skip as well as a full parse.
        let asked = std::cell::Cell::new(0);
        let cancel = || {
            asked.set(asked.get() + 1);
            true
        };
        assert!(fields_cancellable(text.as_bytes(), WANTED, &cancel).unwrap_err().to_string().contains("cancelled"));
        assert_eq!(asked.get(), 1, "stopped at the first check");
        Ok(())
    }
}
