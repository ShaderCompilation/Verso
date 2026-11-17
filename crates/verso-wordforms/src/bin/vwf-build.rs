//! Builds a word-form pack from the OpenCorpora export and checks it.
//!
//! ```text
//! bzcat dict.opcorpora.xml.bz2 | vwf-build --out ru.vwf [--encoding rules|sets] [--date 2024-04-23] [--check list.tsv]
//! vwf-build --pack ru.vwf --check list.tsv
//! vwf-build --pack ru.vwf --compare other.vwf
//! ```
//!
//! The check list has one form per line: `form<TAB>lemma, lemma…` with the
//! expected lemmas best first, or `form<TAB>-` for a form that must stay
//! unknown. A third column is a note; `#` starts a comment line.
use anyhow::{Context, Result, bail, ensure};
use std::{
    io::{self, BufReader},
    path::{Path, PathBuf},
    time::Instant,
};
use verso_wordforms::{Encoding, Pack, fold, opencorpora::Dictionary, write::Writer};

const USAGE: &str =
    "usage: vwf-build --out PACK [--encoding rules|sets] [--date DATE] [--check LIST] < dict.opcorpora.xml
       vwf-build --pack PACK --check LIST
       vwf-build --pack PACK --compare OTHER";

fn main() {
    if let Err(e) = run() {
        eprintln!("vwf-build: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut out = None;
    let mut pack = None;
    let mut check = None;
    let mut compare = None;
    let mut encoding = Encoding::Rules;
    let mut date = String::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().with_context(|| format!("{arg} needs a value\n{USAGE}"));
        match arg.as_str() {
            "--out" => out = Some(PathBuf::from(value()?)),
            "--pack" => pack = Some(PathBuf::from(value()?)),
            "--check" => check = Some(PathBuf::from(value()?)),
            "--compare" => compare = Some(PathBuf::from(value()?)),
            "--date" => date = value()?,
            "--encoding" => {
                encoding = match value()?.as_str() {
                    "sets" => Encoding::Sets,
                    "rules" => Encoding::Rules,
                    other => bail!("Unknown encoding {other:?}\n{USAGE}"),
                }
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            other => bail!("Unknown argument {other:?}\n{USAGE}"),
        }
    }
    let path = match (out, pack) {
        (Some(out), None) => {
            build(&out, encoding, &date)?;
            out
        }
        (None, Some(pack)) => {
            ensure!(check.is_some() || compare.is_some(), "--pack needs --check or --compare\n{USAGE}");
            pack
        }
        _ => bail!("Give either --out or --pack\n{USAGE}"),
    };
    if let Some(other) = compare {
        compare_packs(&Pack::open(&path)?, &Pack::open(&other)?)?;
    }
    if let Some(list) = check {
        let pack = Pack::open(&path)?;
        let started = Instant::now();
        let ok = run_check(&pack, &list)?;
        println!("Checked in {:.1} ms", started.elapsed().as_secs_f64() * 1000.0);
        if !ok {
            println!("(the check reports; it doesn't fail the build)");
        }
    }
    Ok(())
}

fn build(out: &Path, encoding: Encoding, date: &str) -> Result<()> {
    let started = Instant::now();
    let dict = Dictionary::read(BufReader::with_capacity(1 << 20, io::stdin().lock()))?;
    let read = started.elapsed();
    let revision = dict.revision.clone();
    let source = format!("OpenCorpora {}", dict.version);
    let mut meta = vec![("language", "ru"), ("source", source.as_str()), ("revision", revision.as_str())];
    if !date.is_empty() {
        meta.push(("date", date));
    }
    let mut writer = Writer::new(encoding, &meta)?;
    let summary = dict.write(&mut writer)?;
    let (bytes, stats) = writer.finish()?;
    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let partial = out.with_extension("vwf.partial");
    std::fs::write(&partial, &bytes).with_context(|| format!("Can't write {}", partial.display()))?;
    std::fs::rename(&partial, out)?;
    let total = started.elapsed();

    let mb = |n: u64| n as f64 / 1_000_000.0;
    println!("{source}, revision {revision}{}", if date.is_empty() { String::new() } else { format!(", {date}") });
    println!(
        "Lexemes {}, forms read {}, skipped {} (not a single word), keys {} ({} ambiguous, {} truncated)",
        summary.lexemes,
        summary.forms,
        summary.skipped_forms,
        summary.keys,
        summary.ambiguous_keys,
        summary.truncated_keys
    );
    let list = |links: &std::collections::BTreeMap<String, usize>| {
        links.iter().map(|(name, n)| format!("{name} {n}")).collect::<Vec<_>>().join(", ")
    };
    println!("Links followed: {}", list(&summary.followed_links));
    println!("Links ignored: {}", list(&summary.ignored_links));
    println!("Dangling links: {}", summary.dangling_links);
    println!(
        "Encoding {}: {:.2} MB ({} bytes) = map {:.2} MB + values {:.2} MB ({} distinct) + strings {:.2} MB",
        encoding.name(),
        mb(stats.total_bytes),
        stats.total_bytes,
        mb(stats.map_bytes),
        mb(stats.value_bytes),
        stats.values,
        mb(stats.string_bytes)
    );
    println!(
        "Built {} in {:.1} s (XML read {:.1} s), peak memory {}",
        out.display(),
        total.as_secs_f64(),
        read.as_secs_f64(),
        peak_memory().unwrap_or_else(|| "unknown".into())
    );
    Ok(())
}

/// Checks that two packs (say, the two encodings) give the same lemmas for
/// every form.
fn compare_packs(a: &Pack, b: &Pack) -> Result<()> {
    ensure!(a.len() == b.len(), "{} forms against {}", a.len(), b.len());
    let started = Instant::now();
    a.for_each(|key, lemmas| {
        let other = b.lemmas(key)?.with_context(|| format!("{key}: missing from the other pack"))?;
        ensure!(lemmas == other, "{key}: {lemmas:?} against {other:?}");
        Ok(())
    })?;
    println!(
        "Same lemmas for all {} forms ({} against {}), {:.1} s",
        a.len(),
        a.encoding().name(),
        b.encoding().name(),
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

/// Peak resident memory on Linux.
fn peak_memory() -> Option<String> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(format!("{} MB", kb / 1000))
}

/// Runs the quality list. Lemmas that fold like the form itself are skipped,
/// as lookup does: the exact word was already tried. `false` when anything
/// is wrong.
fn run_check(pack: &Pack, list: &Path) -> Result<bool> {
    let text = std::fs::read_to_string(list).with_context(|| format!("Can't read {}", list.display()))?;
    let (mut positive, mut found, mut first_right, mut top_three, mut negative, mut unknown) = (0, 0, 0, 0, 0, 0);
    let mut problems = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim_end();
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split('\t');
        let form = fields.next().unwrap_or_default().trim();
        let expected = fields.next().with_context(|| format!("{}:{}: no expected lemmas", list.display(), n + 1))?;
        let key = fold(form);
        let got: Vec<String> =
            pack.lemmas(form)?.unwrap_or_default().into_iter().filter(|lemma| fold(lemma) != key).collect();
        let shown = if got.is_empty() { "nothing".to_owned() } else { got.join(", ") };
        if expected.trim() == "-" {
            negative += 1;
            if got.is_empty() {
                unknown += 1;
            } else {
                problems.push(format!("false hit       {form}: got {shown}"));
            }
            continue;
        }
        let expected: Vec<&str> = expected.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
        ensure!(!expected.is_empty(), "{}:{}: no expected lemmas", list.display(), n + 1);
        positive += 1;
        if got.is_empty() {
            problems.push(format!("not found       {form}: expected {}", expected.join(", ")));
            continue;
        }
        found += 1;
        top_three += usize::from(got.iter().take(3).any(|g| g == expected[0]));
        if got[0] == expected[0] {
            first_right += 1;
        } else {
            problems.push(format!("wrong first     {form}: expected {}, got {shown}", expected.join(", ")));
            continue;
        }
        let missing: Vec<&str> = expected.iter().copied().filter(|e| !got.iter().any(|g| g == e)).collect();
        if !missing.is_empty() {
            problems.push(format!("missing lemma   {form}: no {}, got {shown}", missing.join(", ")));
        }
    }
    let pct = |a: usize, b: usize| if b == 0 { 100.0 } else { a as f64 * 100.0 / b as f64 };
    println!("Check {}:", list.display());
    println!(
        "  inflected forms {positive}: found {found} ({:.1}%), right first lemma {first_right} ({:.1}%), \
         expected lemma in the first three {top_three} ({:.1}%)",
        pct(found, positive),
        pct(first_right, positive),
        pct(top_three, positive)
    );
    println!("  negative cases {negative}: unknown {unknown}");
    for problem in &problems {
        println!("  {problem}");
    }
    Ok(problems.is_empty())
}
