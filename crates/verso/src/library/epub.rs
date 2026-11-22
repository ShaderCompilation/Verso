//! Bounded, in-memory EPUB package inspection. Never invokes crengine or extracts files.
use super::Metadata;
use anyhow::{Context, Result, ensure};
use quick_xml::{Reader, events::Event};
use std::collections::BTreeMap;
use std::io::{Read, Seek};
use std::path::Path;
use zip::ZipArchive;

pub const XML_LIMIT: u64 = 2 * 1024 * 1024;
pub const IMAGE_LIMIT: u64 = 12 * 1024 * 1024;

pub fn entry<R: Read + Seek>(zip: &mut ZipArchive<R>, name: &str, limit: u64) -> Result<Vec<u8>> {
    let file = zip.by_name(name).with_context(|| format!("missing EPUB entry {name}"))?;
    ensure!(file.size() <= limit, "EPUB entry exceeds {limit} bytes");
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= limit, "EPUB entry exceeds {limit} bytes");
    Ok(bytes)
}

#[derive(Default, Debug)]
pub(super) struct Node {
    pub(super) name: String,
    attrs: BTreeMap<String, String>,
    text: String,
    children: Vec<Node>,
}
impl Node {
    pub(super) fn attr(&self, name: &str) -> &str {
        self.attrs.get(name).map(String::as_str).unwrap_or_default()
    }
    pub(super) fn find(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|n| n.name == name)
    }
    pub(super) fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> {
        self.children.iter().filter(move |n| n.name == name)
    }
}
fn local(name: &[u8]) -> String {
    String::from_utf8_lossy(name.rsplit(|&b| b == b':').next().unwrap_or_default()).into_owned()
}
fn xml(bytes: &[u8]) -> Result<Node> {
    let mut reader = Reader::from_reader(bytes);
    let mut stack = vec![Node::default()];
    let mut count = 0;
    loop {
        let event = reader.read_event()?;
        let empty = matches!(&event, Event::Empty(_));
        match event {
            Event::Start(e) | Event::Empty(e) => {
                count += 1;
                ensure!(count <= 20_000 && stack.len() <= 64, "EPUB XML is too complex");
                let mut node = Node { name: local(e.name().as_ref()), ..Node::default() };
                for attr in e.attributes() {
                    let attr = attr?;
                    node.attrs.insert(
                        local(attr.key.as_ref()),
                        attr.decoded_and_normalized_value(quick_xml::XmlVersion::Implicit1_0, reader.decoder())?
                            .into_owned(),
                    );
                }
                if empty {
                    if node.name == "br" && stack[1..].iter().any(|n| n.name == "description") {
                        stack.last_mut().unwrap().text.push('\n');
                    }
                    stack.last_mut().unwrap().children.push(node);
                } else {
                    stack.push(node);
                }
            }
            Event::End(_) => {
                ensure!(stack.len() > 1, "unexpected XML end");
                let node = stack.pop().unwrap();
                // Text fields keep the text of elements inside them; a
                // description written as XHTML keeps its paragraph breaks.
                if stack[1..].iter().any(|n| matches!(n.name.as_str(), "title" | "creator" | "language" | "identifier"))
                {
                    stack.last_mut().unwrap().text.push_str(&node.text);
                } else if stack[1..].iter().any(|n| n.name == "description") {
                    let parent = stack.last_mut().unwrap();
                    parent.text.push_str(&node.text);
                    match node.name.as_str() {
                        "p" | "div" | "blockquote" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "li" => {
                            parent.text.push_str("\n\n")
                        }
                        "br" => parent.text.push('\n'),
                        _ => {}
                    }
                }
                stack.last_mut().unwrap().children.push(node);
            }
            Event::Text(e) => {
                stack.last_mut().unwrap().text.push_str(&e.xml_content(quick_xml::XmlVersion::Implicit1_0)?)
            }
            Event::CData(e) => {
                stack.last_mut().unwrap().text.push_str(&e.xml_content(quick_xml::XmlVersion::Implicit1_0)?)
            }
            Event::GeneralRef(e) => {
                let value = e.decode()?;
                let replacement = quick_xml::escape::unescape(&format!("&{value};"))?.into_owned();
                stack.last_mut().unwrap().text.push_str(&replacement);
            }
            Event::DocType(e) => ensure!(!e.as_ref().contains(&b'['), "EPUB XML entity declarations are unsupported"),
            Event::Eof => break,
            _ => {}
        }
    }
    ensure!(stack.len() == 1 && stack[0].children.len() == 1, "incomplete EPUB XML");
    Ok(stack.pop().unwrap().children.remove(0))
}

/// Resolve an archive-local URI, including percent-encoded UTF-8 and dot segments.
pub fn resolve(base: &str, reference: &str) -> Result<String> {
    let reference = reference.split(['#', '?']).next().unwrap_or_default();
    ensure!(!reference.contains(':') && !reference.starts_with('/'), "external cover resource");
    let mut decoded = Vec::new();
    let mut chars = reference.as_bytes().iter().copied();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let a = chars.next().context("invalid URI escape")?;
            let b = chars.next().context("invalid URI escape")?;
            decoded.push(
                (a as char).to_digit(16).context("invalid URI escape")? as u8 * 16
                    + (b as char).to_digit(16).context("invalid URI escape")? as u8,
            );
        } else {
            decoded.push(b);
        }
    }
    let decoded = String::from_utf8(decoded)?;
    ensure!(!decoded.contains([':', '\\', '\0']) && !decoded.starts_with('/'), "invalid archive path");
    let mut parts: Vec<_> = base.rsplit_once('/').map_or(Vec::new(), |(p, _)| p.split('/').collect());
    for part in decoded.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                ensure!(parts.pop().is_some(), "archive path escapes root");
            }
            _ => parts.push(part),
        }
    }
    ensure!(!parts.is_empty(), "empty archive path");
    Ok(parts.join("/"))
}

fn text(node: Option<&Node>) -> String {
    node.map(|n| n.text.split_whitespace().collect::<Vec<_>>().join(" ")).unwrap_or_default()
}
fn cover_page<R: Read + Seek>(zip: &mut ZipArchive<R>, path: &str) -> Option<String> {
    let page = xml(&entry(zip, path, XML_LIMIT).ok()?).ok()?;
    fn image(node: &Node) -> Option<&str> {
        match node.name.as_str() {
            "img" => Some(node.attr("src")),
            "image" => Some(node.attr("href")),
            _ => node.children.iter().find_map(image),
        }
    }
    resolve(path, image(&page)?).ok()
}

/// Opens an EPUB archive, refusing ones with an unreasonable number of entries.
pub(super) fn open(path: &Path) -> Result<ZipArchive<std::fs::File>> {
    let zip = ZipArchive::new(std::fs::File::open(path)?)?;
    ensure!(zip.len() <= 50_000, "too many EPUB entries");
    Ok(zip)
}

/// The package document (OPF): its path in the archive and its parsed XML.
pub(super) fn package<R: Read + Seek>(zip: &mut ZipArchive<R>) -> Result<(String, Node)> {
    let container = xml(&entry(zip, "META-INF/container.xml", XML_LIMIT)?)?;
    let rootfiles = container.find("rootfiles").context("EPUB has no rootfiles")?;
    let rootfile = rootfiles
        .all("rootfile")
        .find(|n| n.attr("media-type") == "application/oebps-package+xml")
        .or_else(|| rootfiles.find("rootfile"))
        .context("EPUB has no package")?;
    let package_path = resolve("", rootfile.attr("full-path"))?;
    let package = xml(&entry(zip, &package_path, XML_LIMIT)?)?;
    Ok((package_path, package))
}

/// Reads package metadata from an EPUB archive without filling an empty title
/// from the filename. Callers reading a temporary `.part` file fill the title
/// from the final filename after renaming.
pub fn read_metadata(path: &Path) -> Result<Metadata> {
    let mut zip = open(path)?;
    let (package_path, package) = package(&mut zip)?;
    let mut result = Metadata { synopsis: Some(false), ..Metadata::default() };
    let mut cover_id = "";
    if let Some(meta) = package.find("metadata") {
        let identifier = package.attr("unique-identifier");
        if !identifier.is_empty() {
            result.identifier = text(meta.all("identifier").find(|n| n.attr("id") == identifier));
        }
        result.title = text(meta.find("title"));
        result.authors =
            meta.all("creator").map(|n| text(Some(n))).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", ");
        result.language = text(meta.find("language"));
        result.synopsis = Some(meta.all("description").any(|n| has_text(&n.text)));
        cover_id = meta.all("meta").find(|n| n.attr("name") == "cover").map_or("", |n| n.attr("content"));
    }
    let mut candidates = Vec::new();
    if let Some(manifest) = package.find("manifest") {
        for item in manifest.all("item").filter(|n| n.attr("properties").split_whitespace().any(|s| s == "cover-image"))
        {
            candidates.push((item.attr("href"), false));
        }
        if !cover_id.is_empty()
            && let Some(item) = manifest.all("item").find(|n| n.attr("id") == cover_id)
        {
            candidates.push((item.attr("href"), !item.attr("media-type").starts_with("image/")));
        }
    }
    if let Some(guide) = package.find("guide") {
        for item in guide.all("reference").filter(|n| n.attr("type").split_whitespace().any(|s| s == "cover")) {
            candidates.push((item.attr("href"), true));
        }
    }
    for (href, page) in candidates {
        let Ok(mut cover) = resolve(&package_path, href) else { continue };
        if page {
            // Some EPUB 2 guides link directly to the image.
            let is_image = cover
                .rsplit('.')
                .next()
                .is_some_and(|ext| ["jpg", "jpeg", "png", "svg"].iter().any(|x| ext.eq_ignore_ascii_case(x)));
            if !is_image {
                let Some(image) = cover_page(&mut zip, &cover) else { continue };
                cover = image;
            }
        }
        if zip.by_name(&cover).is_ok() {
            result.cover = Some(cover);
            break;
        }
    }
    Ok(result)
}

/// Whether a description shows any text once its markup is left out.
fn has_text(description: &str) -> bool {
    let mut tag = false;
    description.chars().any(|c| {
        match c {
            '<' => tag = true,
            '>' => tag = false,
            _ => return !tag && c.is_alphanumeric(),
        }
        false
    })
}

/// The book's description (synopsis) as written in the package: usually
/// HTML, sometimes plain text with blank lines between paragraphs. `None`
/// when it has no text.
pub fn synopsis(path: &Path) -> Result<Option<String>> {
    let mut zip = open(path)?;
    let (_, package) = package(&mut zip)?;
    Ok(package
        .find("metadata")
        .and_then(|meta| meta.all("description").find(|n| has_text(&n.text)))
        .map(|n| n.text.trim().to_owned()))
}

pub fn read(path: &Path) -> Result<Metadata> {
    let mut result = read_metadata(path)?;
    if result.title.is_empty() {
        result.title = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    }
    Ok(result)
}

/// Self-contained vector covers only. Resource references and filters fall back
/// rather than reading external files or allocating large intermediate surfaces.
pub fn validate_svg(bytes: &[u8]) -> Result<()> {
    fn validate(node: &Node) -> Result<()> {
        ensure!(
            !matches!(node.name.as_str(), "image" | "filter" | "foreignObject" | "text"),
            "unsupported SVG resource"
        );
        for (key, value) in &node.attrs {
            if key == "href" {
                ensure!(value.starts_with('#'), "external SVG resource");
            }
        }
        for child in &node.children {
            validate(child)?;
        }
        Ok(())
    }
    validate(&xml(bytes)?)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;
    pub fn fixture(path: &Path, opf: &str, extra: &[(&str, &[u8])]) -> Result<()> {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path)?);
        let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in [
            ("META-INF/container.xml", br#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OPS/book.opf"/></rootfiles></container>"#.as_slice()),
            ("OPS/book.opf", opf.as_bytes()),
        ].into_iter().chain(extra.iter().copied()) {
            zip.start_file(name, options)?;
            zip.write_all(data)?;
        }
        zip.finish()?;
        Ok(())
    }
    #[test]
    fn epub2_and_3_metadata_and_cover_paths() -> Result<()> {
        let path = std::env::temp_dir().join(format!("verso-metadata-{}.epub", std::process::id()));
        fixture(
            &path,
            r#"<package><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Граф &amp; 世界</dc:title><dc:creator>One</dc:creator><dc:creator>Two</dc:creator><dc:language>ru</dc:language><meta name="cover" content="old"/></metadata><manifest><item id="old" href="old.png" media-type="image/png"/><item href="../images/a%20b.png" properties="nav cover-image"/></manifest></package>"#,
            &[("images/a b.png", b"png"), ("OPS/old.png", b"old")],
        )?;
        let m = read(&path)?;
        assert_eq!(m.title, "Граф & 世界");
        assert_eq!(m.authors, "One, Two");
        assert_eq!(m.cover.as_deref(), Some("images/a b.png"));
        fixture(
            &path,
            r#"<package><metadata><meta name="cover" content="old"/></metadata><manifest><item id="old" href="old.png" media-type="image/png"/></manifest></package>"#,
            &[("OPS/old.png", b"old")],
        )?;
        assert_eq!(read(&path)?.cover.as_deref(), Some("OPS/old.png"));
        fixture(
            &path,
            r#"<package><guide><reference type="cover" href="cover.xhtml"/></guide></package>"#,
            &[("OPS/cover.xhtml", br#"<!DOCTYPE html PUBLIC 'example' 'https://example.invalid/test.dtd'><html><body><img src="../cover.png"/></body></html>"#), ("cover.png", b"png")],
        )?;
        assert_eq!(read(&path)?.cover.as_deref(), Some("cover.png"));
        fixture(&path, "<package/>", &[])?;
        assert_eq!(read(&path)?.title, path.file_stem().unwrap().to_string_lossy());
        fixture(&path, "<package>", &[])?;
        assert!(read(&path).is_err());
        std::fs::write(&path, b"not an EPUB")?;
        assert!(read(&path).is_err());
        std::fs::remove_file(path)?;
        Ok(())
    }
    #[test]
    fn bounded_xml_and_local_paths() -> Result<()> {
        assert!(resolve("OPS/book.opf", "../../no.png").is_err());
        assert!(resolve("OPS/book.opf", "https://example.com/a.png").is_err());
        assert!(resolve("", "%2fetc/passwd").is_err());
        assert_eq!(resolve("OPS/book.opf", "../a%20b.png#fragment")?, "a b.png");
        assert!(xml(b"<!DOCTYPE package [<!ENTITY x 'boom'>]><package/>").is_err());
        assert!(xml(b"<!DOCTYPE html PUBLIC 'example' 'https://example.invalid/test.dtd'><html/>").is_ok());
        assert!(xml("<a>".repeat(70).as_bytes()).is_err());
        let mut data = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut data);
            zip.start_file("huge", zip::write::SimpleFileOptions::default())?;
            zip.write_all(&[0; 1025])?;
            zip.finish()?;
        }
        assert!(entry(&mut ZipArchive::new(data)?, "huge", 1024).is_err());
        Ok(())
    }
    #[test]
    fn synopses_from_escaped_html_xhtml_and_plain_text() -> Result<()> {
        let path = std::env::temp_dir().join(format!("verso-synopsis-{}.epub", std::process::id()));
        let opf = |description: &str| {
            format!(
                r#"<package><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>T</dc:title>{description}</metadata></package>"#
            )
        };
        // Calibre and most stores escape the HTML.
        fixture(
            &path,
            &opf("<dc:description>&lt;p&gt;A &lt;i&gt;sweeping&lt;/i&gt; saga.&lt;/p&gt;</dc:description>"),
            &[],
        )?;
        assert_eq!(read(&path)?.synopsis, Some(true));
        assert_eq!(synopsis(&path)?.as_deref(), Some("<p>A <i>sweeping</i> saga.</p>"));
        // XHTML elements keep their text and paragraph breaks.
        fixture(
            &path,
            &opf(
                r#"<dc:description><div xmlns="http://www.w3.org/1999/xhtml"><p>One <i>two</i>.</p><p>Three<br/>four</p></div></dc:description>"#,
            ),
            &[],
        )?;
        assert_eq!(synopsis(&path)?.as_deref(), Some("One two.\n\nThree\nfour"));
        // An empty or markup-only description, or none, is no synopsis.
        for description in
            ["<dc:description>  </dc:description>", "<dc:description>&lt;p&gt; &lt;/p&gt;</dc:description>", ""]
        {
            fixture(&path, &opf(description), &[])?;
            assert_eq!(read(&path)?.synopsis, Some(false), "{description}");
            assert_eq!(synopsis(&path)?, None);
        }
        // The first description with text wins.
        fixture(&path, &opf("<dc:description/><dc:description>Plain.\n\nSecond.</dc:description>"), &[])?;
        assert_eq!(synopsis(&path)?.as_deref(), Some("Plain.\n\nSecond."));
        // A package without metadata has been read, not left unknown.
        fixture(&path, "<package/>", &[])?;
        assert_eq!(read(&path)?.synopsis, Some(false));
        std::fs::remove_file(path)?;
        Ok(())
    }
}
