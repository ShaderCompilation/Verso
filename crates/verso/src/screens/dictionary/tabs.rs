//! The lookup card's tabs, and what the online ones asked for the word shown.
//! A tab asks when it's first shown, never before, and keeps its reply until
//! the word changes; everything goes when the card does.
use super::{plain, translate::Translate, wikipedia::Wikipedia};
use crate::{
    geom::Rect,
    gfx::StyledBlock,
    online::{Error, QUERY_LIMIT, Service, Worker, translate::Provider, wikipedia::Edition},
    platform::Waker,
    ui::{
        gesture::Gesture,
        theme::{INK2, Theme},
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

/// Whether a query is too long for Wikipedia, and to be edited. Not for the
/// dictionaries: names of chemicals can be this long.
pub fn too_long(query: &str) -> bool {
    query.trim().chars().count() > QUERY_LIMIT
}

/// What the online tabs look things up in.
pub struct Languages {
    pub wikipedia: Edition,
    /// The Wikipedia in the book's language, if there is one.
    pub book: Option<Edition>,
    /// What Translate translates from; None detects it. Only for this card.
    pub source: Option<&'static str>,
    /// None until the reader chooses; nothing is translated before.
    pub target: Option<&'static str>,
    /// The book's language, if it can be translated.
    pub written: Option<&'static str>,
    /// Translation backend and optional DeepL API key (from Settings).
    pub provider: Provider,
    pub deepl_api_key: Option<String>,
}

/// One request and what came of it.
#[derive(Default)]
pub enum Slot<T> {
    #[default]
    Unasked,
    /// The request's ID.
    Waiting(u64),
    Done(Result<T, Error>),
}

impl<T> Slot<T> {
    pub fn waiting(&self) -> bool {
        matches!(self, Slot::Waiting(_))
    }

    pub fn waits_for(&self, request: u64) -> bool {
        matches!(self, Slot::Waiting(id) if *id == request)
    }

    pub fn error(&self) -> Option<&Error> {
        if let Slot::Done(Err(e)) = self { Some(e) } else { None }
    }

    /// Keeps a finished reply only: the request behind another is cancelled.
    pub fn finished(self) -> Self {
        if matches!(self, Slot::Done(_)) { self } else { Slot::Unasked }
    }
}

/// Both online tabs' replies for one word.
#[derive(Default)]
pub struct Replies {
    wikipedia: Wikipedia,
    translate: Translate,
}

impl Replies {
    pub fn is_empty(&self) -> bool {
        self.wikipedia.is_unasked() && self.translate.is_unasked()
    }

    /// After another Wikipedia was chosen.
    pub fn forget_wikipedia(&mut self) {
        self.wikipedia = Wikipedia::default();
    }

    /// After another language to translate from or into was chosen.
    pub fn forget_translation(&mut self) {
        self.translate = Translate::default();
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
        self.bar.disabled = if too_long(query) { vec![Tab::Wikipedia.index()] } else { Vec::new() };
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
            Tab::Wikipedia => {
                if let Some(request) = self.replies.wikipedia.wanted(self.languages.wikipedia, query) {
                    self.replies.wikipedia.asked(self.wikipedia.request(request));
                }
            }
            Tab::Translate => self.replies.translate.ask(&self.translate, &self.languages, query),
            Tab::Dictionary => {}
        }
    }

    /// Takes in replies; true when the tab shown has a new one.
    pub fn receive(&mut self) -> bool {
        let mut shown = false;
        if let Some(reply) = self.wikipedia.receive()
            && self.replies.wikipedia.receive(reply)
        {
            shown |= self.current == Tab::Wikipedia;
        }
        if let Some(reply) = self.translate.receive()
            && self.replies.translate.receive(reply)
        {
            shown |= self.current == Tab::Translate;
        }
        shown
    }

    /// What Wikipedia has for the word shown.
    pub fn wikipedia(&self) -> &Wikipedia {
        &self.replies.wikipedia
    }

    pub fn translate(&self) -> &Translate {
        &self.replies.translate
    }

    pub fn languages(&self) -> &Languages {
        &self.languages
    }

    /// Opens one of Wikipedia's matches; `ask` then asks for its article.
    /// False when it's open already.
    pub fn open_match(&mut self, index: usize) -> bool {
        let wikipedia = &mut self.replies.wikipedia;
        if wikipedia.opened() == Some(index) && wikipedia.error().is_none() {
            return false;
        }
        // Another article may be on its way.
        self.wikipedia.cancel();
        wikipedia.open(index);
        true
    }

    /// Looks words up in another Wikipedia; `ask` then asks it.
    pub fn set_edition(&mut self, edition: Edition) {
        self.languages.wikipedia = edition;
        self.wikipedia.cancel();
        self.replies.forget_wikipedia();
    }

    /// Translates from another language (None detects it); `ask` then asks.
    pub fn set_source(&mut self, source: Option<&'static str>) {
        self.languages.source = source;
        self.translate.cancel();
        self.replies.forget_translation();
    }

    /// Translates into another language; `ask` then asks.
    pub fn set_target(&mut self, target: &'static str) {
        self.languages.target = Some(target);
        self.translate.cancel();
        self.replies.forget_translation();
    }

    /// Translate shows, and what to translate into isn't chosen yet.
    pub fn needs_target(&self) -> bool {
        self.current == Tab::Translate && self.languages.target.is_none()
    }

    /// Translate shows, and the text is in the language to translate into.
    pub fn needs_another_language(&self) -> bool {
        self.current == Tab::Translate && self.replies.translate.needs_another_language(&self.languages)
    }

    /// Why the tab shown has no answer.
    pub fn error(&self) -> Option<&Error> {
        match self.current {
            Tab::Dictionary => None,
            Tab::Wikipedia => self.replies.wikipedia.error(),
            Tab::Translate => self.replies.translate.error(),
        }
    }

    /// Drops the tab's error, so `ask` asks again.
    pub fn retry(&mut self) {
        match self.current {
            Tab::Dictionary => {}
            Tab::Wikipedia => self.replies.wikipedia.retry(),
            Tab::Translate => self.replies.forget_translation(),
        }
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
            Tab::Wikipedia => self.replies.wikipedia.waiting(),
            Tab::Translate => self.replies.translate.waiting(),
        }
    }

    /// What an online tab shows, apart from its lists. Its words can't be
    /// looked up.
    pub fn content(&self, t: &Theme) -> Vec<StyledBlock> {
        match self.current {
            Tab::Dictionary => Vec::new(),
            Tab::Wikipedia => self.replies.wikipedia.content(self.languages.wikipedia, t),
            Tab::Translate => self.replies.translate.content(&self.languages, t),
        }
    }
}

#[cfg(test)]
impl Languages {
    /// The English Wikipedia, in a book written in `book`.
    pub fn for_test(book: Option<&str>, target: Option<&'static str>) -> Self {
        let written = book.and_then(crate::online::translate::language);
        Self {
            wikipedia: Edition::ENGLISH,
            book: book.and_then(Edition::for_language),
            source: None,
            target,
            written,
            provider: Provider::Google,
            deepl_api_key: None,
        }
    }
}

#[cfg(test)]
impl Tabs {
    pub fn set_languages(&mut self, languages: Languages) {
        self.languages = languages;
    }

    /// Tabs with Wi-Fi off.
    pub fn offline(current: Tab) -> Self {
        use crate::platform::wifi::Link;
        let service =
            Service::for_test("http://127.0.0.1:9", std::time::Duration::from_secs(1), Box::new(|| Ok(Link::Off)));
        Self::new(current, &service, Waker::new(|| {}), Languages::for_test(None, None))
    }
}

pub fn message(title: &str, text: &str, t: &Theme) -> Vec<StyledBlock> {
    vec![plain(title.into(), t.button, true), plain(text.into(), t.body.gray(INK2), true)]
}
