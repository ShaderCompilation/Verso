//! The lookup card's tabs, and what the online ones asked for the word shown.
//! A tab asks when it's first shown, never before, and keeps its reply until
//! the word changes; everything goes when the card does.
use super::{plain, wikipedia::Wikipedia};
use crate::{
    geom::Rect,
    gfx::StyledBlock,
    online::{Answer, Error, QUERY_LIMIT, Request, Service, Worker, wikipedia::Edition},
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

/// Whether a query is too long for anything but Translate.
pub fn too_long(query: &str) -> bool {
    query.trim().chars().count() > QUERY_LIMIT
}

/// What the online tabs look things up in.
pub struct Languages {
    pub wikipedia: Edition,
    /// The Wikipedia in the book's language, if there is one.
    pub book: Option<Edition>,
    /// None until the reader chooses; nothing is translated before.
    pub translate: Option<String>,
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
    fn finished(self) -> Self {
        if matches!(self, Slot::Done(_)) { self } else { Slot::Unasked }
    }
}

/// Both online tabs' replies for one word.
#[derive(Default)]
pub struct Replies {
    wikipedia: Wikipedia,
    translate: Slot<Answer>,
}

impl Replies {
    pub fn is_empty(&self) -> bool {
        self.wikipedia.is_unasked() && matches!(self.translate, Slot::Unasked)
    }

    /// After another Wikipedia was chosen.
    pub fn forget_wikipedia(&mut self) {
        self.wikipedia = Wikipedia::default();
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
            Tab::Wikipedia => {
                if let Some(request) = self.replies.wikipedia.wanted(self.languages.wikipedia, query) {
                    self.replies.wikipedia.asked(self.wikipedia.request(request));
                }
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
        if let Some(reply) = self.wikipedia.receive()
            && self.replies.wikipedia.receive(reply)
        {
            shown |= self.current == Tab::Wikipedia;
        }
        if let Some(reply) = self.translate.receive()
            && self.replies.translate.waits_for(reply.id)
        {
            self.replies.translate = Slot::Done(reply.result);
            shown |= self.current == Tab::Translate;
        }
        shown
    }

    /// What Wikipedia has for the word shown.
    pub fn wikipedia(&self) -> &Wikipedia {
        &self.replies.wikipedia
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
            Tab::Translate => self.replies.translate = Slot::Unasked,
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

    /// What an online tab shows, apart from Wikipedia's list of matches. Its
    /// words can't be looked up.
    pub fn content(&self, t: &Theme) -> Vec<StyledBlock> {
        let line = |s: &str| vec![plain(s.into(), t.body, true)];
        match self.current {
            Tab::Dictionary => Vec::new(),
            Tab::Wikipedia => self.replies.wikipedia.content(self.languages.wikipedia, t),
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
        let languages = Languages { wikipedia: Edition::ENGLISH, book: None, translate: None };
        Self::new(current, &service, Waker::new(|| {}), languages)
    }
}

pub fn message(title: &str, text: &str, t: &Theme) -> Vec<StyledBlock> {
    vec![plain(title.into(), t.button, true), plain(text.into(), t.body.gray(INK2), true)]
}
