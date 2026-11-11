//! The only thread that touches crengine. Results carry a book/layout generation.
use super::{CreDocument, CreEngine, Document, FontFace, Layout, Metadata, Paths, Position, TocEntry, Typography};
use crate::platform::Waker;
use anyhow::Result;
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;

#[derive(Clone, Debug)]
pub struct Page {
    pub generation: u64,
    pub number: usize,
    pub pixels: Vec<u8>,
    pub position: Position,
    pub progress: u32,
    pub images: bool,
}
pub enum Command {
    Open { generation: u64, path: PathBuf, layout: Layout, typography: Typography, footer: bool, position: Position },
    Layout { generation: u64, typography: Typography, footer: bool, position: Position },
    Page { generation: u64, number: usize },
    Position { generation: u64, position: Position },
    Close,
    Shutdown,
}
pub enum Reply {
    Fonts(Vec<FontFace>),
    Opened { generation: u64, metadata: Metadata, toc: Vec<TocEntry>, pages: usize, current: usize },
    Page(Arc<Page>),
    Error { generation: u64, message: String },
}
pub struct Worker {
    tx: mpsc::Sender<Command>,
    rx: RefCell<mpsc::Receiver<Reply>>,
    join: Option<JoinHandle<()>>,
}
impl Worker {
    pub fn new(paths: Paths, dpi: u32, waker: Waker) -> Self {
        let (tx, commands) = mpsc::channel();
        let (results, rx) = mpsc::channel();
        let join = std::thread::Builder::new()
            .name("verso-book".into())
            .spawn(move || {
                let send = |r| {
                    let _ = results.send(r);
                    waker.wake();
                };
                let engine = CreEngine::new(paths, dpi);
                let engine = match engine {
                    Ok(e) => e,
                    Err(e) => {
                        send(Reply::Error { generation: 0, message: format!("Book engine: {e:#}") });
                        return;
                    }
                };
                send(Reply::Fonts(engine.fonts()));
                let mut doc: Option<CreDocument> = None;
                let mut generation = 0;
                let mut dimensions = (0, 0);
                let mut pending = None;
                while let Some(command) = pending.take().or_else(|| commands.recv().ok()) {
                    // Skip intermediate layouts already queued, preserving ordering of other commands.
                    let command = coalesce(command, &commands, &mut pending);
                    if matches!(command, Command::Shutdown) {
                        break;
                    }
                    let result: Result<()> = (|| {
                        match command {
                            Command::Open { generation: g, path, layout, typography, footer, position } => {
                                generation = g;
                                doc = None;
                                dimensions = (layout.width, layout.height);
                                let mut d = engine.open(&path, &layout, Some((&typography, footer)))?;
                                if !position.0.is_empty()
                                    && let Err(e) = d.go_to_position(&position)
                                {
                                    log::warn!("{e}");
                                }
                                doc = Some(d);
                            }
                            Command::Layout { generation: g, typography, footer, position } => {
                                generation = g;
                                if let Some(d) = &mut doc {
                                    d.go_to_position(&position)?;
                                    d.set_typography(&typography, dpi, footer)?;
                                }
                            }
                            Command::Page { generation: g, number } => {
                                if g == generation
                                    && let Some(d) = &mut doc
                                {
                                    send(Reply::Page(render(d, g, number, dimensions)?));
                                }
                                return Ok(());
                            }
                            Command::Position { generation: g, position } => {
                                if g == generation
                                    && let Some(d) = &mut doc
                                {
                                    d.go_to_position(&position)?;
                                    let number = d.current_page();
                                    send(Reply::Page(render(d, g, number, dimensions)?));
                                }
                                return Ok(());
                            }
                            Command::Close => {
                                doc = None;
                                return Ok(());
                            }
                            Command::Shutdown => return Ok(()),
                        }
                        if let Some(d) = &mut doc {
                            let pages = d.page_count();
                            let current = d.current_page();
                            let metadata = d.metadata();
                            log::info!(
                                "book {:?}, authors {:?}, language {:?}",
                                metadata.title,
                                metadata.authors,
                                metadata.language
                            );
                            send(Reply::Opened { generation, metadata, toc: d.toc()?, pages, current });
                            send(Reply::Page(render(d, generation, current, dimensions)?));
                        }
                        Ok(())
                    })();
                    if let Err(e) = result {
                        send(Reply::Error { generation, message: format!("{e:#}") });
                    }
                }
            })
            .expect("start book thread");
        Self { tx, rx: RefCell::new(rx), join: Some(join) }
    }
    pub fn send(&self, c: Command) {
        if self.tx.send(c).is_err() {
            log::error!("book thread stopped");
        }
    }
    pub fn drain(&self) -> Vec<Reply> {
        self.rx.borrow_mut().try_iter().collect()
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}
fn coalesce(mut c: Command, rx: &mpsc::Receiver<Command>, pending: &mut Option<Command>) -> Command {
    while matches!(c, Command::Layout { .. }) {
        match rx.try_recv() {
            Ok(next @ Command::Layout { .. }) => c = next,
            Ok(next) => {
                *pending = Some(next);
                break;
            }
            Err(_) => break,
        }
    }
    c
}
fn render(d: &mut CreDocument, generation: u64, number: usize, (w, h): (u32, u32)) -> Result<Arc<Page>> {
    let started = std::time::Instant::now();
    let last = d.page_count().saturating_sub(1);
    d.go_to_page(number.min(last));
    let mut pixels = vec![255; w as usize * h as usize];
    d.draw(&mut pixels, w as usize)?;
    let page = Page {
        generation,
        number: d.current_page(),
        pixels,
        position: d.position()?,
        progress: d.progress(),
        images: d.has_images(),
    };
    log::info!("page {} rendered in {:?}", page.number + 1, started.elapsed());
    Ok(Arc::new(page))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn layouts_coalesce_without_discarding_navigation() {
        let (tx, rx) = mpsc::channel();
        let layout = |generation| Command::Layout {
            generation,
            typography: Typography::default(),
            footer: true,
            position: Position::default(),
        };
        tx.send(layout(2)).unwrap();
        tx.send(Command::Page { generation: 2, number: 9 }).unwrap();
        let mut pending = None;
        assert!(matches!(coalesce(layout(1), &rx, &mut pending), Command::Layout { generation: 2, .. }));
        assert!(matches!(pending, Some(Command::Page { number: 9, .. })));
    }
}
