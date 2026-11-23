//! Builds and checks Verso releases. scripts/package.sh and scripts/deploy.sh
//! run it.
//!
//! ```text
//! verso-release pack --components dist/components.json --stage DIR --out DIR --version 0.3.0 --released 2026-12-01 --notes FILE
//! verso-release installed --components dist/components.json --stage DIR --version 0.3.0 [--dev] --out FILE
//! verso-release trusted-comment --manifest latest.json
//! verso-release verify --release DIR [--key FILE]...
//! ```
//!
//! `pack` writes one archive per component, `latest.json` and the
//! first-install zip. `installed` writes an `installed.json` for the staged
//! files and prints `id hash` lines. `trusted-comment` prints what the
//! signature must carry. `verify` checks a signed release folder with the keys
//! built into this binary, or with `--key` files.
use anyhow::{Context, Result, bail, ensure};
use std::{fs, path::PathBuf};
use verso_update::{
    Manifest, Version,
    release::{self, PackOptions},
    release_key_names, release_keys, trusted_comment,
};

const USAGE: &str =
    "usage: verso-release pack --components FILE --stage DIR --out DIR --version V --released DATE --notes FILE
       verso-release installed --components FILE --stage DIR --version V [--dev] --out FILE
       verso-release trusted-comment --manifest FILE
       verso-release verify --release DIR [--key FILE]...";

fn main() {
    if let Err(e) = run() {
        eprintln!("verso-release: {e:#}");
        std::process::exit(1);
    }
}

#[derive(Default)]
struct Args {
    components: Option<PathBuf>,
    stage: Option<PathBuf>,
    out: Option<PathBuf>,
    version: Option<Version>,
    released: Option<String>,
    notes: Option<PathBuf>,
    manifest: Option<PathBuf>,
    release: Option<PathBuf>,
    keys: Vec<PathBuf>,
    dev: bool,
}

fn run() -> Result<()> {
    let mut argv = std::env::args().skip(1);
    let command = argv.next().context(USAGE)?;
    let mut args = Args::default();
    while let Some(arg) = argv.next() {
        let mut value = || argv.next().with_context(|| format!("{arg} needs a value\n{USAGE}"));
        match arg.as_str() {
            "--components" => args.components = Some(value()?.into()),
            "--stage" => args.stage = Some(value()?.into()),
            "--out" => args.out = Some(value()?.into()),
            "--version" => args.version = Some(value()?.parse()?),
            "--released" => args.released = Some(value()?),
            "--notes" => args.notes = Some(value()?.into()),
            "--manifest" => args.manifest = Some(value()?.into()),
            "--release" => args.release = Some(value()?.into()),
            "--key" => args.keys.push(value()?.into()),
            "--dev" => args.dev = true,
            other => bail!("unknown argument {other:?}\n{USAGE}"),
        }
    }
    let need = |v: Option<PathBuf>, name: &str| v.with_context(|| format!("{command} needs --{name}\n{USAGE}"));
    match command.as_str() {
        "pack" => {
            let specs = release::read_specs(&need(args.components, "components")?)?;
            let notes_path = need(args.notes, "notes")?;
            let notes = fs::read_to_string(&notes_path).with_context(|| format!("reading {}", notes_path.display()))?;
            let stage = need(args.stage, "stage")?;
            let out = need(args.out, "out")?;
            let manifest = release::pack(&PackOptions {
                specs: &specs,
                stage: &stage,
                out: &out,
                version: args.version.context("pack needs --version")?,
                released: args.released.context("pack needs --released")?,
                notes,
            })?;
            for c in &manifest.components {
                println!("{:<14} {:>9} bytes  {}", c.id, c.archive.size, &c.hash[..16]);
            }
        }
        "installed" => {
            let specs = release::read_specs(&need(args.components, "components")?)?;
            let version = args.version.context("installed needs --version")?;
            let installed = release::installed(&specs, &need(args.stage, "stage")?, version, args.dev)?;
            let mut json = serde_json::to_vec_pretty(&installed)?;
            json.push(b'\n');
            fs::write(need(args.out, "out")?, json)?;
            for c in &installed.components {
                println!("{} {}", c.id, c.hash);
            }
        }
        "trusted-comment" => {
            // Validated, so a malformed manifest is never signed.
            let manifest = Manifest::parse(&fs::read(need(args.manifest, "manifest")?)?)?;
            println!("{}", trusted_comment(&manifest.version));
        }
        "verify" => {
            let dir = need(args.release, "release")?;
            let files: Vec<String> = args
                .keys
                .iter()
                .map(|k| fs::read_to_string(k).with_context(|| k.display().to_string()))
                .collect::<Result<_>>()?;
            let (keys, from): (Vec<&str>, String) = if files.is_empty() {
                let names = release_key_names();
                ensure!(!names.is_empty(), "no release keys are built in: add crates/verso-update/keys/main.pub");
                (release_keys(), format!("built-in keys ({})", names.join(", ")))
            } else {
                (files.iter().map(String::as_str).collect(), "the given keys".into())
            };
            let manifest = release::verify_release(&dir, &keys)?;
            println!(
                "{} {}: signature and {} archives verified with {from}",
                dir.display(),
                manifest.version,
                manifest.components.len()
            );
        }
        "-h" | "--help" => println!("{USAGE}"),
        other => bail!("unknown command {other:?}\n{USAGE}"),
    }
    Ok(())
}
