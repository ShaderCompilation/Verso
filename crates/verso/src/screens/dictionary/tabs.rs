//! The lookup card's tabs, and what the online ones asked for the word shown.
//! A tab asks when it's first shown, never before, and keeps its reply until
//! the word changes; everything goes when the card does.
use super::plain;
use crate::{
    dictionary::languages::language_name,
    geom::Rect,
    gfx::{StyledBlock, Weight},
    online::{
        Answer, Error, QUERY_LIMIT, Request, Service, Worker,
        wikipedia::{Article, Block, Edition, Found},
    },
    platform::Waker,
    ui::{
        gesture::Gesture,
        theme::{INK2, LINE, Theme},
        widget::DrawCx,
        widgets::Segmented,
    },
};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Dictionary,
    Wikipedia,
    Translate,
}

impl Tab {
    const ALL: [Tab; 3] = [Tab::Dictionary, Tab::Wikipedia, Tab::Translate];

    pub fn index(self) -> usize {
        self as usize
    }
}

/// Whether a query is too long for anything but Translate.
pub fn too_long(query: &str) -> bool {
    query.trim().chars().count() > QUERY_LIMIT
}

/// What the online tabs look things up in.
pub struct Languages {
    pub wikipedia: Edition,
    /// None until the reader chooses; nothing is translated before.
    pub translate: Option<String>,
}

/// What an online tab knows about the word shown.
#[derive(Default)]
enum Slot {
    #[default]
    Unasked,
    /// The request's ID.
    Waiting(u64),
    Done(Result<Answer, Error>),
}

impl Slot {
    /// Keeps a finished reply only: the request behind another is cancelled.
    fn finished(self) -> Self {
        if matches!(self, Slot::Done(_)) { self } else { Slot::Unasked }
    }
}

/// Both online tabs' replies for one word.
#[derive(Default)]
pub struct Replies {
    wikipedia: Slot,
    translate: Slot,
}

impl Replies {
    pub fn is_empty(&self) -> bool {
        matches!((&self.wikipedia, &self.translate), (Slot::Unasked, Slot::Unasked))
    }
}

pub struct Tabs {
    current: Tab,
    bar: Segmented,
    languages: Languages,
    wikipedia: Worker,
    translate: Worker,
    replies: Replies,
}

impl Tabs {
    pub fn new(current: Tab, service: &Arc<Service>, waker: Waker, languages: Languages) -> Self {
        Self {
            current,
            bar: Segmented::texts(&["Dictionary", "Wikipedia", "Translate"], current.index()),
            languages,
            wikipedia: Worker::new(service.clone(), waker.clone()),
            translate: Worker::new(service.clone(), waker),
            replies: Replies::default(),
        }
    }

    pub fn current(&self) -> Tab {
        self.current
    }

    #[cfg(test)]
    pub fn rect(&self) -> Rect {
        self.bar.rect()
    }

    /// Shows a tab; `ask` then asks what it needs.
    pub fn show(&mut self, tab: Tab) {
        self.current = tab;
        self.bar.selected = tab.index();
    }

    /// Places the row (empty while it's hidden) for a query.
    pub fn place(&mut self, rect: Rect, query: &str) {
        self.bar.place(rect);
        let disabled = [Tab::Dictionary.index(), Tab::Wikipedia.index()];
        self.bar.disabled = if too_long(query) { disabled.to_vec() } else { Vec::new() };
    }

    /// The other tab, once it's tapped.
    pub fn on_gesture(&mut self, g: &Gesture) -> Option<Tab> {
        self.bar.on_gesture(g).map(|i| Tab::ALL[i])
    }

    pub fn draw(&self, cx: &mut DrawCx) {
        self.bar.draw(cx);
    }

    /// Asks for the current tab's answer unless it already did.
    pub fn ask(&mut self, query: &str) {
        let query = query.trim();
        if query.is_empty() {
            return;
        }
        match self.current {
            Tab::Wikipedia if matches!(self.replies.wikipedia, Slot::Unasked) => {
                let request = Request::Wikipedia { edition: self.languages.wikipedia, query: query.into() };
                self.replies.wikipedia = Slot::Waiting(self.wikipedia.request(request));
            }
            Tab::Translate if matches!(self.replies.translate, Slot::Unasked) => {
                let Some(target) = self.languages.translate.clone() else { return };
                let request = Request::Translate { text: query.into(), source: None, target };
                self.replies.translate = Slot::Waiting(self.translate.request(request));
            }
            _ => {}
        }
    }

    /// Takes in replies; true when the tab shown has a new one.
    pub fn receive(&mut self) -> bool {
        let mut shown = false;
        for (tab, worker, slot) in [
            (Tab::Wikipedia, &self.wikipedia, &mut self.replies.wikipedia),
            (Tab::Translate, &self.translate, &mut self.replies.translate),
        ] {
            if let Some(reply) = worker.receive()
                && matches!(slot, Slot::Waiting(id) if *id == reply.id)
            {
                *slot = Slot::Done(reply.result);
                shown |= tab == self.current;
            }
        }
        shown
    }

    /// Hands over the replies before the word changes, and cancels what's
    /// still on its way.
    pub fn take_replies(&mut self) -> Replies {
        self.wikipedia.cancel();
        self.translate.cancel();
        let Replies { wikipedia, translate } = std::mem::take(&mut self.replies);
        Replies { wikipedia: wikipedia.finished(), translate: translate.finished() }
    }

    /// Goes back to an earlier word's tab and replies.
    pub fn restore(&mut self, tab: Tab, replies: Replies) {
        self.take_replies();
        self.replies = replies;
        self.show(tab);
    }

    /// The tab shown waits for its answer.
    pub fn waiting(&self) -> bool {
        match self.current {
            Tab::Dictionary => false,
            Tab::Wikipedia => matches!(self.replies.wikipedia, Slot::Waiting(_)),
            Tab::Translate => matches!(self.replies.translate, Slot::Waiting(_)),
        }
    }

    /// What an online tab shows. Its words can't be looked up.
    pub fn content(&self, t: &Theme) -> Vec<StyledBlock> {
        let line = |s: &str| vec![plain(s.into(), t.body, true)];
        match self.current {
            Tab::Dictionary => Vec::new(),
            Tab::Wikipedia => match &self.replies.wikipedia {
                Slot::Unasked => line("Edit the words above to look them up."),
                Slot::Waiting(_) => {
                    let language = language_name(self.languages.wikipedia.code());
                    line(&format!("Searching {language} Wikipedia…"))
                }
                Slot::Done(Ok(Answer::Found(Found::Article { article, .. }) | Answer::Article(article))) => {
                    article_blocks(article, t)
                }
                Slot::Done(Ok(Answer::Found(Found::Hits(hits)))) if !hits.is_empty() => {
                    hits.iter().map(|hit| plain(hit.title.clone(), t.dictionary_body, true)).collect()
                }
                Slot::Done(Ok(_)) => {
                    message("No article found", "Edit the words above, or choose another language.", t)
                }
                Slot::Done(Err(e)) => message(&e.title("Wikipedia"), &e.plain(), t),
            },
            Tab::Translate => match &self.replies.translate {
                _ if self.languages.translate.is_none() => line("Translate into which language?"),
                Slot::Unasked => line("Edit the words above to translate them."),
                Slot::Waiting(_) => line("Translating…"),
                Slot::Done(Ok(Answer::Translation(translation))) => {
                    let gap = t.dictionary_body.line_height() / 2.0;
                    vec![
                        plain(translation.text.clone(), t.dictionary_body, true),
                        StyledBlock::Gap(gap),
                        plain("Google Translate".into(), t.small.gray(INK2), true),
                    ]
                }
                Slot::Done(Ok(_)) => message("No translation", "The answer couldn’t be read.", t),
                Slot::Done(Err(e)) => message(&e.title("Google Translate"), &e.plain(), t),
            },
        }
    }
}

#[cfg(test)]
impl Tabs {
    /// Tabs with Wi-Fi off.
    pub fn offline(current: Tab) -> Self {
        use crate::platform::wifi::Link;
        let service =
            Service::for_test("http://127.0.0.1:9", std::time::Duration::from_secs(1), Box::new(|| Ok(Link::Off)));
        let languages = Languages { wikipedia: Edition::ENGLISH, translate: None };
        Self::new(current, &service, Waker::new(|| {}), languages)
    }
}

fn message(title: &str, text: &str, t: &Theme) -> Vec<StyledBlock> {
    vec![plain(title.into(), t.button, true), plain(text.into(), t.body.gray(INK2), true)]
}

fn article_blocks(article: &Article, t: &Theme) -> Vec<StyledBlock> {
    let gap = t.dictionary_body.line_height() / 2.0;
    let mut blocks = vec![plain(article.title.clone(), t.dictionary_headword, true)];
    for block in &article.blocks {
        blocks.push(StyledBlock::Gap(gap));
        blocks.push(match block {
            Block::Heading { text, .. } => plain(text.clone(), t.dictionary_body.weight(Weight::SemiBold), true),
            Block::Paragraph(text) => plain(text.clone(), t.dictionary_body, true),
        });
    }
    blocks.push(StyledBlock::Rule { space: gap, thickness: t.hairline, gray: LINE });
    let source = format!("Wikipedia · {} · CC BY-SA 4.0", language_name(article.edition.code()));
    for text in [article.url.clone(), source] {
        blocks.push(plain(text, t.small.gray(INK2), true));
    }
    blocks
}
