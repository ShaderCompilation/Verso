//! KOReader's reading progress, read one way: KOReader → Verso. Only
//! `Book.sdr/metadata.epub.lua` beside the book and KOReader's `history.lua` are read; Verso never writes either.
use crate::annotations::lua;
use crate::document::Position;
use crate::store::BookState;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

pub mod bulk;

/// The last KOReader position offered, taken or imported for a book.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    /// The sidecar's modified time, in seconds.
    pub modified: u64,
    pub position: Position,
}

/// KOReader's `summary.status`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Reading,
    Complete,
    Abandoned,
    /// Missing (older sidecars have no summary) or a value Verso doesn't know.
    Other,
}

/// What Verso reads from a book's KOReader sidecar.
#[derive(Clone, Debug, PartialEq)]
pub struct Progress {
    /// `last_xpointer`: a crengine XPointer, the format Verso saves too.
    pub position: Position,
    /// `percent_finished`, 0–1; 0 when missing.
    pub percent: f64,
    pub status: Status,
    /// The file's modified time, in seconds.
    pub modified: u64,
    /// `summary.modified`, the day KOReader last changed `summary`, as local
    /// midnight (Unix seconds). None when missing or not a date.
    pub status_day: Option<u64>,
}
impl Progress {
    /// The percentage as Verso's `BookState::progress` (0–10000).
    pub fn progress(&self) -> u32 {
        (self.percent * 10000.0).round() as u32
    }
    pub fn seen(&self) -> Seen {
        Seen { modified: self.modified, position: self.position.clone() }
    }
}

/// The book's `metadata.epub.lua` inside its `Book.sdr` sidecar. Each
/// format keeps its own file there, so a sidecar shared with `Book.pdf`
/// works too. None for books that aren't EPUBs.
pub fn metadata_path(book: &Path) -> Option<PathBuf> {
    let ext = book.extension()?.to_str()?;
    ext.eq_ignore_ascii_case("epub").then(|| crate::library::files::sidecar(book).join(format!("metadata.{ext}.lua")))
}

/// No such file: no sidecar folder, or a plain file where it would be.
fn absent(e: &std::io::Error) -> bool {
    matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory)
}

fn seconds(meta: &std::fs::Metadata) -> Result<u64> {
    Ok(meta.modified()?.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()))
}

/// The metadata file's modified time, in seconds: the cheap check before
/// reading it. None when there is no such file.
pub fn modified(path: &Path) -> Result<Option<u64>> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => Ok(Some(seconds(&meta)?)),
        Ok(_) => Ok(None),
        Err(e) if absent(&e) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// What the bulk import reads from a sidecar (§5).
#[derive(Clone, Debug, PartialEq)]
pub enum Sidecar {
    Progress(Progress),
    /// No position, but `summary.status` is `"complete"`: the book was
    /// marked finished in KOReader without being read there. `modified`
    /// and `status_day` as in [`Progress`].
    Complete {
        modified: u64,
        status_day: Option<u64>,
    },
}
impl Sidecar {
    /// For a `complete` sidecar: when KOReader last saved it, and the
    /// finished date to give the book (`summary.modified`, else the file's
    /// time).
    pub fn completed(&self) -> Option<(u64, u64)> {
        let (modified, day) = match self {
            Sidecar::Progress(p) if p.status == Status::Complete => (p.modified, p.status_day),
            Sidecar::Progress(_) => return None,
            Sidecar::Complete { modified, status_day } => (*modified, *status_day),
        };
        Some((modified, day.unwrap_or(modified)))
    }
}

/// The position of a metadata file, for the check on open (§4). None when
/// there is no file or it has no position.
pub fn read(path: &Path, cancelled: &dyn Fn() -> bool) -> Result<Option<Progress>> {
    Ok(match read_sidecar(path, cancelled)? {
        Some(Sidecar::Progress(p)) => Some(p),
        Some(Sidecar::Complete { .. }) | None => None,
    })
}

/// Reads the position fields of a metadata file, skipping everything else
/// without building it. None when there is no file, or it has neither a
/// position nor a `complete` status. Works for KOReader's legacy format too:
/// the fields are top-level in both.
pub fn read_sidecar(path: &Path, cancelled: &dyn Fn() -> bool) -> Result<Option<Sidecar>> {
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if absent(&e) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Ok(None);
    }
    let modified = seconds(&meta)?;
    let mut bytes = Vec::new();
    file.by_ref().take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    let fields = lua::fields_cancellable(&bytes, &["last_xpointer", "percent_finished", "summary"], cancelled)?;
    let percent = match fields.get("percent_finished") {
        Some(lua::Value::Scalar(s)) => s.parse::<f64>().unwrap_or(0.0).clamp(0.0, 1.0),
        _ => 0.0,
    };
    let summary = fields.get("summary").and_then(|s| s.table().ok());
    let status_day = summary.and_then(|s| s.get("modified")?.string()).and_then(crate::clock::LocalDateTime::midnight);
    let status = match summary.and_then(|s| s.get("status")?.string()) {
        Some("reading") => Status::Reading,
        Some("complete") => Status::Complete,
        Some("abandoned") => Status::Abandoned,
        _ => Status::Other,
    };
    let Some(position) = fields.get("last_xpointer").and_then(lua::Value::string).filter(|p| !p.is_empty()) else {
        return Ok((status == Status::Complete).then_some(Sidecar::Complete { modified, status_day }));
    };
    Ok(Some(Sidecar::Progress(Progress {
        position: Position(position.to_owned()),
        percent,
        status,
        modified,
        status_day,
    })))
}

/// When KOReader last opened each file, from its `history.lua` (a list of
/// `{ file = "/mnt/us/…", time = <unix seconds> }`). Empty when there is no
/// such file; entries without a path or a time are skipped.
pub fn history(path: &Path, cancelled: &dyn Fn() -> bool) -> Result<HashMap<PathBuf, u64>> {
    let mut bytes = Vec::new();
    match std::fs::File::open(path) {
        Ok(f) => f.take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes)?,
        Err(e) if absent(&e) => return Ok(HashMap::new()),
        Err(e) => return Err(e.into()),
    };
    let data = lua::parse_cancellable(&bytes, cancelled)?;
    let mut times = HashMap::new();
    for entry in data.table()?.values().filter_map(|v| v.table().ok()) {
        let file = entry.get("file").and_then(lua::Value::string).filter(|f| !f.is_empty());
        let time = match entry.get("time") {
            Some(lua::Value::Scalar(s)) => s.parse::<u64>().ok(),
            _ => None,
        };
        if let (Some(file), Some(time)) = (file, time) {
            let at = times.entry(PathBuf::from(file)).or_insert(time);
            *at = (*at).max(time);
        }
    }
    Ok(times)
}

/// §3 rule 2: the sidecar changed after Verso's last save for the book and
/// after the KOReader position Verso last dealt with. Decides, from a `stat`
/// alone, whether the file is worth reading.
pub fn newer(state: &BookState, modified: u64) -> bool {
    modified > state.last_opened && state.koreader_seen.as_ref().is_none_or(|s| modified > s.modified)
}

/// What opening a book does with KOReader's position (§3). The setting (rule
/// 1) is the caller's: off, nothing is checked at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnOpen {
    /// Nothing to offer and nothing to record.
    Nothing,
    /// A new position, but exactly where Verso is: record it, don't ask.
    Record,
    /// Rules 2 and 3 hold: offer it if it falls on another page (rule 4,
    /// which needs the laid-out book). On the same page, record it.
    Offer,
}
pub fn on_open(state: &BookState, progress: &Progress) -> OnOpen {
    // Rule 3: KOReader rewrites the sidecar when a book is only opened and
    // closed, so a newer file doesn't always hold a new position.
    let dealt_with = state.koreader_seen.as_ref().is_some_and(|s| s.position == progress.position);
    if !newer(state, progress.modified) || dealt_with {
        OnOpen::Nothing
    } else if progress.position == state.position {
        OnOpen::Record
    } else {
        OnOpen::Offer
    }
}

/// The check on open (§4), on the reader's I/O thread: a `stat`, and the
/// position fields only when the file is newer (rule 2). None when there is
/// nothing to offer or record, including books without a sidecar.
pub fn check(path: &Path, state: &BookState, cancelled: &dyn Fn() -> bool) -> Result<Option<(OnOpen, Progress)>> {
    let Some(modified) = modified(path)? else { return Ok(None) };
    if !newer(state, modified) {
        return Ok(None);
    }
    let Some(progress) = read(path, cancelled)? else { return Ok(None) };
    Ok(match on_open(state, &progress) {
        OnOpen::Nothing => None,
        found => Some((found, progress)),
    })
}

/// How the bulk import counts a book (§5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Found {
    /// Verso has no position: imported.
    NotStarted,
    /// KOReader's is newer and elsewhere: imported.
    NewerInKoreader,
    /// The same position as Verso's or the one already dealt with.
    UpToDate,
    /// Verso's position is kept.
    NewerInVerso,
    /// No position in KOReader, only a `complete` status, that marks the
    /// book finished (see [`marks_finished`]). Otherwise the book is
    /// `UpToDate`.
    CompleteOnly,
}

/// The bulk import's decision for one book.
#[derive(Clone, Debug)]
pub struct Import {
    pub found: Found,
    /// The state to write, or None when nothing changes.
    pub state: Option<BookState>,
}

/// Whether a `complete` sidecar KOReader saved at `modified` marks the
/// book finished. Finished is a lasting label, so
/// KOReader's must be newer than Verso's last change to it (`status_at`): a
/// book marked unfinished in Verso since is left alone. A book being re-read
/// in Verso is only marked again when KOReader saved after Verso's last save
/// too, so an old sidecar doesn't end the re-read. Already finished and not
/// re-read, there is nothing to change.
pub fn marks_finished(state: &BookState, modified: u64) -> bool {
    if state.done() {
        return false;
    }
    modified > state.status_at && (!state.rereading || modified > state.last_opened)
}

/// Decides one book of the bulk import. `opened` is KOReader's time for the
/// file from `history.lua`. An imported position replaces Verso's, with
/// `last_opened` in KOReader's order so Continue reading and Recent follow
/// it. A `complete` status marks the book finished whatever happens to its
/// position, and for a sidecar with no position at all, when
/// [`marks_finished`] allows it; the finished date is KOReader's. Nothing is
/// ever unmarked, and `abandoned` is ignored (Verso has no such state).
pub fn import(state: &BookState, sidecar: &Sidecar, opened: Option<u64>) -> Import {
    let finish = sidecar.completed().filter(|&(modified, _)| marks_finished(state, modified)).map(|(_, at)| at);
    let progress = match sidecar {
        Sidecar::Progress(p) => p,
        Sidecar::Complete { .. } => {
            let Some(at) = finish else { return Import { found: Found::UpToDate, state: None } };
            let mut next = state.clone();
            next.mark_finished(true, at);
            return Import { found: Found::CompleteOnly, state: Some(next) };
        }
    };
    let found = if state.position.0.is_empty() {
        Found::NotStarted
    } else if state.position == progress.position
        || state.koreader_seen.as_ref().is_some_and(|s| s.position == progress.position)
    {
        Found::UpToDate
    } else if newer(state, progress.modified) {
        Found::NewerInKoreader
    } else {
        Found::NewerInVerso
    };
    let mut next = state.clone();
    let mut changed = false;
    if matches!(found, Found::NotStarted | Found::NewerInKoreader) {
        next.position = progress.position.clone();
        next.progress = progress.progress();
        // Unknown until the first open lays the book out.
        next.page = 0;
        next.last_opened = opened.unwrap_or(progress.modified);
        // Read since in KOReader, like a fresh open in Verso.
        next.hidden_from_continue = false;
        next.koreader_seen = Some(progress.seen());
        changed = true;
    }
    if let Some(at) = finish {
        next.mark_finished(true, at);
        changed = true;
    }
    Import { found, state: changed.then_some(next) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotations::lua::tests::sidecar;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("verso-koreader-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
    fn progress(position: &str, modified: u64) -> Progress {
        Progress {
            position: Position(position.into()),
            percent: 0.5,
            status: Status::Reading,
            modified,
            status_day: None,
        }
    }
    fn imp(state: &BookState, progress: &Progress, opened: Option<u64>) -> Import {
        import(state, &Sidecar::Progress(progress.clone()), opened)
    }
    fn state(position: &str, last_opened: u64) -> BookState {
        BookState { position: Position(position.into()), page: 7, progress: 1200, last_opened, ..Default::default() }
    }

    #[test]
    fn reads_a_sidecar_beside_its_book_including_a_shared_one() -> Result<()> {
        let dir = temp("read");
        let book = dir.join("Book.epub");
        let path = metadata_path(&book).unwrap();
        assert_eq!(path, dir.join("Book.sdr/metadata.epub.lua"));
        assert_eq!(metadata_path(&dir.join("Old.EPUB")), Some(dir.join("Old.sdr/metadata.EPUB.lua")));
        assert_eq!(metadata_path(&dir.join("Book.pdf")), None, "EPUB only");
        // A book without a sidecar, and a plain file where the folder would be.
        assert_eq!(modified(&path)?, None);
        assert_eq!(read(&path, &|| false)?, None);
        std::fs::write(dir.join("Book.sdr"), b"not KOReader's")?;
        assert_eq!((modified(&path)?, read(&path, &|| false)?), (None, None));
        std::fs::remove_file(dir.join("Book.sdr"))?;
        // Shared with Book.pdf: each format has its own file inside.
        std::fs::create_dir(dir.join("Book.sdr"))?;
        std::fs::write(dir.join("Book.pdf"), b"pdf")?;
        std::fs::write(
            dir.join("Book.sdr/metadata.pdf.lua"),
            "return { last_xpointer = '/pdf', percent_finished = 0.9 }",
        )?;
        std::fs::write(&path, sidecar(20))?;
        let p = read(&path, &|| false)?.unwrap();
        assert_eq!(p.position.0, "/body/DocFragment[12]/body/p[5]/text().17");
        assert_eq!((p.percent, p.progress(), p.status), (0.6215, 6215, Status::Reading));
        assert_eq!(Some(p.modified), modified(&path)?);
        assert!(p.modified > 1_700_000_000, "seconds since the epoch");
        // Legacy, finished and abandoned, odd values.
        std::fs::write(&path, "return { highlight = {}, last_xpointer = '/l', percent_finished = 1.5 }")?;
        assert_eq!(read(&path, &|| false)?.map(|p| (p.percent, p.status)), Some((1.0, Status::Other)));
        std::fs::write(&path, "return { last_xpointer = '/c', summary = { status = 'complete' } }")?;
        assert_eq!(read(&path, &|| false)?.map(|p| (p.percent, p.status)), Some((0.0, Status::Complete)));
        std::fs::write(
            &path,
            "return { last_xpointer = '/c', summary = { modified = '2026-11-20', status = 'complete' } }",
        )?;
        let p = read(&path, &|| false)?.unwrap();
        assert_eq!(p.status_day, crate::clock::LocalDateTime::midnight("2026-11-20"));
        assert!(p.status_day.is_some());
        std::fs::write(&path, "return { summary = { modified = 'soon', status = 'complete' } }")?;
        assert!(matches!(read_sidecar(&path, &|| false)?, Some(Sidecar::Complete { status_day: None, .. })));
        std::fs::write(&path, "return { summary = { modified = '2026-11-20', status = 'complete' } }")?;
        let day = crate::clock::LocalDateTime::midnight("2026-11-20");
        assert!(
            matches!(read_sidecar(&path, &|| false)?, Some(Sidecar::Complete { status_day, .. }) if status_day == day)
        );
        std::fs::write(&path, "return { last_xpointer = '/a', summary = { status = 'abandoned' } }")?;
        assert_eq!(read(&path, &|| false)?.unwrap().status, Status::Abandoned);
        // No position: nothing to offer or import.
        for text in
            ["return { percent_finished = 0.3 }", "return { last_xpointer = '' }", "return { last_xpointer = 5 }"]
        {
            std::fs::write(&path, text)?;
            assert_eq!(read(&path, &|| false)?, None, "{text}");
        }
        // Marked complete without a position: only the bulk import uses it.
        for text in [
            "return { summary = { status = 'complete' } }",
            "return { last_xpointer = '', percent_finished = 0.2, summary = { status = 'complete' } }",
        ] {
            std::fs::write(&path, text)?;
            assert_eq!(read(&path, &|| false)?, None, "{text}");
            assert_eq!(
                read_sidecar(&path, &|| false)?,
                Some(Sidecar::Complete { modified: modified(&path)?.unwrap(), status_day: None }),
                "{text}"
            );
        }
        for text in ["return { summary = { status = 'abandoned' } }", "return { summary = { status = 'reading' } }"] {
            std::fs::write(&path, text)?;
            assert_eq!(read_sidecar(&path, &|| false)?, None, "{text}");
        }
        std::fs::write(&path, "return { last_xpointer = '/c', summary = { status = 'complete' } }")?;
        assert!(matches!(read_sidecar(&path, &|| false)?, Some(Sidecar::Progress(p)) if p.position.0 == "/c"));
        std::fs::write(&path, "return { last_xpointer = os.getenv('x') }")?;
        assert!(read(&path, &|| false).is_err(), "an unreadable sidecar is an error");
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn history_times_by_path() -> Result<()> {
        let dir = temp("history");
        let path = dir.join("history.lua");
        assert!(history(&path, &|| false)?.is_empty(), "no history file");
        std::fs::write(
            &path,
            r#"return {
    [1] = { ["file"] = "/mnt/us/books/A.epub", ["time"] = 1763800000 },
    [2] = { ["file"] = "/mnt/us/books/B.epub", ["time"] = 1763700000 },
    [3] = { ["file"] = "/mnt/us/books/A.epub", ["time"] = 1763600000 },
    [4] = { ["file"] = "/mnt/us/books/C.epub" },
    [5] = { ["time"] = 1763500000 },
}"#,
        )?;
        let times = history(&path, &|| false)?;
        assert_eq!(times.len(), 2);
        assert_eq!(times[Path::new("/mnt/us/books/A.epub")], 1763800000, "the latest open");
        assert_eq!(times[Path::new("/mnt/us/books/B.epub")], 1763700000);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn on_open_rules() {
        // Never opened in Verso: offered (Verso opens at the start).
        assert_eq!(on_open(&BookState::default(), &progress("/k", 100)), OnOpen::Offer);
        // Newer in KOReader, elsewhere: offered.
        assert_eq!(on_open(&state("/v", 50), &progress("/k", 100)), OnOpen::Offer);
        // Newer in Verso: nothing.
        assert_eq!(on_open(&state("/v", 150), &progress("/k", 100)), OnOpen::Nothing);
        assert_eq!(on_open(&state("/v", 100), &progress("/k", 100)), OnOpen::Nothing, "the same second isn't newer");
        // Exactly where Verso is: recorded without asking.
        assert_eq!(on_open(&state("/k", 50), &progress("/k", 100)), OnOpen::Record);
        // Already dealt with, and KOReader rewrote the file without moving.
        let mut seen = state("/v", 50);
        seen.koreader_seen = Some(progress("/k", 100).seen());
        assert_eq!(on_open(&seen, &progress("/k", 100)), OnOpen::Nothing);
        assert_eq!(on_open(&seen, &progress("/k", 300)), OnOpen::Nothing, "rewritten, same position");
        // Read on in KOReader since: offered again.
        assert_eq!(on_open(&seen, &progress("/k2", 300)), OnOpen::Offer);
        // Seen later than the sidecar (e.g. imported, then Verso's clock moved on).
        seen.koreader_seen = Some(Seen { modified: 400, position: Position("/old".into()) });
        assert!(!newer(&seen, 300));
        assert_eq!(on_open(&seen, &progress("/k2", 300)), OnOpen::Nothing);
    }

    #[test]
    fn the_check_on_open_reads_only_newer_sidecars() -> Result<()> {
        let dir = temp("check");
        let path = metadata_path(&dir.join("Book.epub")).unwrap();
        let never = || false;
        // No sidecar: nothing, and no error.
        assert_eq!(check(&path, &BookState::default(), &never)?, None);
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, "return { last_xpointer = '/k', percent_finished = 0.25 }")?;
        let modified = modified(&path)?.unwrap();
        // Never opened in Verso: offered.
        let (found, p) = check(&path, &BookState::default(), &never)?.unwrap();
        assert_eq!((found, p.position.0.as_str(), p.progress()), (OnOpen::Offer, "/k", 2500));
        // Verso saved since: not even read (an unreadable file isn't an error).
        std::fs::write(&path, "return { last_xpointer = os.exit() }")?;
        let modified_now = super::modified(&path)?.unwrap();
        assert_eq!(check(&path, &state("/v", modified_now), &never)?, None);
        assert!(check(&path, &state("/v", modified_now.min(modified) - 1), &never).is_err());
        // Exactly where Verso is: recorded; already dealt with: nothing.
        std::fs::write(&path, "return { last_xpointer = '/v' }")?;
        assert_eq!(check(&path, &state("/v", 0), &never)?.map(|(f, _)| f), Some(OnOpen::Record));
        let mut seen = state("/x", 0);
        seen.koreader_seen = Some(Seen { modified: 0, position: Position("/v".into()) });
        assert_eq!(check(&path, &seen, &never)?, None);
        // Marked complete without a position: nothing to offer.
        std::fs::write(&path, "return { summary = { status = 'complete' } }")?;
        assert_eq!(check(&path, &BookState::default(), &never)?, None);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn bulk_import_rows() {
        // Verso has no position: imported, with KOReader's time from history.
        let mut p = progress("/k", 100);
        p.percent = 0.62;
        let i = imp(&BookState::default(), &p, Some(90));
        assert_eq!(i.found, Found::NotStarted);
        let s = i.state.unwrap();
        assert_eq!((s.position.0.as_str(), s.progress, s.page, s.last_opened), ("/k", 6200, 0, 90));
        assert_eq!(s.koreader_seen, Some(Seen { modified: 100, position: Position("/k".into()) }));
        assert!(!s.finished);
        // Newer in KOReader: imported; without a history entry, the sidecar's time.
        let mut hidden = state("/v", 50);
        hidden.hidden_from_continue = true;
        hidden.reading_speed = Some(crate::reader::speed::ReadingSpeed::default());
        let i = imp(&hidden, &p, None);
        assert_eq!(i.found, Found::NewerInKoreader);
        let s = i.state.unwrap();
        assert_eq!((s.position.0.as_str(), s.page, s.last_opened, s.hidden_from_continue), ("/k", 0, 100, false));
        assert!(s.reading_speed.is_some(), "the rest of the state is kept");
        // Newer in Verso: kept, nothing written.
        let i = imp(&state("/v", 150), &p, Some(90));
        assert_eq!((i.found, i.state.is_none()), (Found::NewerInVerso, true));
        // The same position as Verso's, or as the one already dealt with.
        let i = imp(&state("/k", 50), &p, None);
        assert_eq!((i.found, i.state.is_none()), (Found::UpToDate, true));
        let mut seen = state("/v", 50);
        seen.koreader_seen = Some(progress("/k", 40).seen());
        // KOReader rewrote the file without moving: still up to date.
        let i = imp(&seen, &progress("/k", 300), None);
        assert_eq!((i.found, i.state.is_none()), (Found::UpToDate, true));
        // Finished: marked even when the position is kept or up to date.
        let mut done = progress("/k", 100);
        done.status = Status::Complete;
        for verso in [state("/v", 150), state("/k", 50)] {
            let s = imp(&verso, &done, None).state.expect("marked finished");
            assert!(s.finished);
            assert_eq!(s.position, verso.position);
        }
        let mut finished = state("/v", 150);
        finished.finished = true;
        assert!(imp(&finished, &done, None).state.is_none(), "already finished: nothing to write");
        // Never unmarked, and abandoned is ignored.
        let i = imp(&finished, &progress("/k", 100), None);
        assert!(i.state.is_none());
        let mut abandoned = progress("/k", 100);
        abandoned.status = Status::Abandoned;
        assert!(imp(&state("/v", 150), &abandoned, None).state.is_none());
        let s = imp(&BookState::default(), &abandoned, None).state.unwrap();
        assert!(!s.finished);
    }

    #[test]
    fn a_complete_sidecar_without_a_position_marks_the_book_finished() {
        let complete = Sidecar::Complete { modified: 100, status_day: Some(80) };
        // Never opened in Verso: finished on KOReader's day, nothing else changes.
        let i = import(&BookState::default(), &complete, Some(90));
        assert_eq!(i.found, Found::CompleteOnly);
        let s = i.state.unwrap();
        assert_eq!((s.finished, s.status_at, s.rereading), (true, 80, false));
        assert_eq!((s.position.0.as_str(), s.last_opened, s.koreader_seen), ("", 0, None));
        // Read in Verso: finished, its position and time kept. Without a
        // `summary.modified`, the date is the sidecar's time.
        let verso = state("/v", 150);
        let i = import(&verso, &Sidecar::Complete { modified: 100, status_day: None }, None);
        assert_eq!(i.found, Found::CompleteOnly);
        let s = i.state.unwrap();
        assert_eq!((s.finished, s.status_at), (true, 100));
        assert_eq!((s.position.0.as_str(), s.page, s.progress, s.last_opened), ("/v", 7, 1200, 150));
        // Already finished: up to date, nothing to write.
        let i = import(&s, &complete, None);
        assert_eq!((i.found, i.state.is_none()), (Found::UpToDate, true));
        // Marked unfinished in Verso after KOReader's save: left alone.
        let unmarked = BookState { status_at: 120, ..verso };
        let i = import(&unmarked, &complete, None);
        assert_eq!((i.found, i.state.is_none()), (Found::UpToDate, true));
    }

    #[test]
    fn finished_is_a_lasting_label_across_imports() {
        let mut done = progress("/k", 1000);
        done.status = Status::Complete;
        done.status_day = Some(900);
        // Marked from KOReader, with KOReader's day as the finished date.
        let s = imp(&state("/v", 2000), &done, None).state.expect("marked");
        assert_eq!((s.finished, s.status_at, s.rereading, s.position.0.as_str()), (true, 900, false, "/v"));
        // Running it again: nothing to change.
        assert!(imp(&s, &done, None).state.is_none());
        // Marked unfinished in Verso since: a re-run leaves it alone.
        let mut unmarked = s.clone();
        assert!(unmarked.mark_finished(false, 3000));
        let i = imp(&unmarked, &done, None);
        assert_eq!((i.found, i.state.is_none()), (Found::NewerInVerso, true));
        // KOReader saved the sidecar after that (read there again): marked again.
        let mut again = done.clone();
        again.modified = 4000;
        again.status_day = Some(3600);
        let s = imp(&unmarked, &again, None).state.expect("marked again");
        assert_eq!((s.finished, s.status_at), (true, 3600));
        // Re-reading in Verso since KOReader's save: the re-read goes on.
        let mut rereading = s.clone();
        rereading.rereading = true;
        rereading.last_opened = 5000;
        assert!(imp(&rereading, &again, None).state.is_none());
        // Read and finished in KOReader after the re-read started: finished,
        // the re-read over.
        let mut later = again.clone();
        later.modified = 6000;
        later.status_day = None;
        let s = imp(&rereading, &later, None).state.expect("re-read ended");
        assert_eq!((s.finished, s.rereading, s.status_at), (true, false, 6000));
        // A sidecar that isn't complete never touches the label.
        let reading = progress("/k2", 9000);
        let s = imp(&unmarked, &reading, None).state.expect("a newer position");
        assert_eq!((s.finished, s.status_at), (false, 3000));
        let s = imp(&rereading, &reading, None).state.expect("a newer position");
        assert_eq!((s.finished, s.rereading, s.status_at), (true, true, 3600));
    }
}
