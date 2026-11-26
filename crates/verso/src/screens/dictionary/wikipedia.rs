//! The Wikipedia tab for one word: what the search found, and the article
//! open. A clear best match opens at once; otherwise the matches are a list.
use super::{
    plain,
    tabs::{Slot, message},
};
use crate::{
    gfx::{StyledBlock, StyledParagraph, TextStyle, Weight},
    online::{
        Answer, Error, Reply, Request,
        wikipedia::{Article, Block, Edition, Found, Hit},
    },
    ui::{
        theme::{INK2, LINE, Theme},
        widgets::Row,
    },
};

#[derive(Default)]
pub struct Wikipedia {
    lookup: Slot<()>,
    /// Everything the search found, the best first.
    matches: Vec<Hit>,
    /// The match opened. None: the matches show as a list.
    open: Option<Open>,
    /// The articles opened so far, by their match.
    articles: Vec<(usize, Article)>,
}

struct Open {
    /// In `matches`.
    index: usize,
    /// Once it's here, `articles` has it.
    article: Slot<()>,
}

impl Wikipedia {
    pub fn is_unasked(&self) -> bool {
        matches!(self.lookup, Slot::Unasked)
    }

    pub fn waiting(&self) -> bool {
        self.lookup.waiting() || self.open.as_ref().is_some_and(|o| o.article.waiting())
    }

    /// What has to be asked for next, if anything.
    pub fn wanted(&self, edition: Edition, query: &str) -> Option<Request> {
        if self.is_unasked() {
            return Some(Request::Wikipedia { edition, query: query.into() });
        }
        let open = self.open.as_ref().filter(|o| matches!(o.article, Slot::Unasked))?;
        Some(Request::Article { edition, title: self.matches[open.index].title.clone() })
    }

    /// The wanted request is on its way.
    pub fn asked(&mut self, id: u64) {
        match &mut self.open {
            Some(open) if matches!(open.article, Slot::Unasked) => open.article = Slot::Waiting(id),
            _ => self.lookup = Slot::Waiting(id),
        }
    }

    /// Takes in a reply; false when it isn't waited for.
    pub fn receive(&mut self, reply: Reply) -> bool {
        if self.lookup.waits_for(reply.id) {
            self.lookup = Slot::Done(match reply.result {
                Ok(Answer::Found(found)) => {
                    self.found(found);
                    Ok(())
                }
                Ok(_) => Err(Error::Unreadable),
                Err(e) => Err(e),
            });
            return true;
        }
        let Some(open) = self.open.as_mut().filter(|o| o.article.waits_for(reply.id)) else { return false };
        open.article = Slot::Done(match reply.result {
            Ok(Answer::Article(article)) => {
                self.articles.push((open.index, article));
                Ok(())
            }
            Ok(_) => Err(Error::Unreadable),
            Err(e) => Err(e),
        });
        true
    }

    fn found(&mut self, found: Found) {
        match found {
            Found::Hits(hits) => self.matches = hits,
            Found::Article { article, others } => {
                let start = article.blocks.iter().find_map(|b| match b {
                    Block::Paragraph(text) => Some(text.as_str()),
                    Block::Heading { .. } => None,
                });
                // A row's worth.
                let snippet = start.unwrap_or_default().chars().take(120).collect();
                self.matches = vec![Hit { title: article.title.clone(), redirect: None, snippet }];
                self.matches.extend(others);
                self.articles = vec![(0, article)];
                self.open = Some(Open { index: 0, article: Slot::Done(Ok(())) });
            }
        }
    }

    /// Opens a match; its article is then wanted, unless it was open before.
    pub fn open(&mut self, index: usize) {
        let here = self.articles.iter().any(|(i, _)| *i == index);
        self.open = Some(Open { index, article: if here { Slot::Done(Ok(())) } else { Slot::Unasked } });
    }

    /// The match opened, and the article once it's here.
    pub fn opened(&self) -> Option<usize> {
        self.open.as_ref().map(|o| o.index)
    }

    pub fn matches(&self) -> &[Hit] {
        &self.matches
    }

    /// The matches while they show as a list.
    pub fn listing(&self) -> Option<&[Hit]> {
        (self.open.is_none() && !self.matches.is_empty()).then_some(&self.matches)
    }

    /// The search worked and found nothing.
    pub fn found_nothing(&self) -> bool {
        matches!(self.lookup, Slot::Done(Ok(()))) && self.matches.is_empty()
    }

    pub fn error(&self) -> Option<&Error> {
        self.lookup.error().or_else(|| self.open.as_ref()?.article.error())
    }

    /// Wants what failed again.
    pub fn retry(&mut self) {
        match &mut self.open {
            Some(open) if open.article.error().is_some() => open.article = Slot::Unasked,
            _ if self.lookup.error().is_some() => self.lookup = Slot::Unasked,
            _ => {}
        }
    }

    /// For when the word may come back: what was on its way is wanted again,
    /// and only the open article is kept.
    pub fn finished(mut self) -> Self {
        if self.lookup.waiting() {
            return Self::default();
        }
        if let Some(open) = &mut self.open {
            if open.article.waiting() {
                open.article = Slot::Unasked;
            }
            self.articles.retain(|(i, _)| *i == open.index);
        }
        self
    }

    /// What shows under the tab's row, except the list of matches.
    pub fn content(&self, edition: Edition, t: &Theme) -> Vec<StyledBlock> {
        let line = |s: String| vec![plain(s, t.body, true)];
        if let Some(e) = self.error() {
            return message(&e.title("Wikipedia"), &e.plain(), t);
        }
        match &self.lookup {
            Slot::Unasked => line("Edit the words above to look them up.".into()),
            Slot::Waiting(_) => line(format!("Searching {} Wikipedia…", edition.name())),
            Slot::Done(_) => match &self.open {
                Some(open) => match self.articles.iter().find(|(i, _)| *i == open.index) {
                    Some((_, article)) => article_blocks(article, t),
                    None => line(format!("Opening “{}”…", self.matches[open.index].title)),
                },
                None if self.matches.is_empty() => {
                    message("No article found", "Edit the words above, or choose another language.", t)
                }
                None => Vec::new(),
            },
        }
    }

    /// A match as a row.
    pub fn row(hit: &Hit) -> Row {
        let row = Row::new(&hit.title);
        if hit.snippet.is_empty() { row } else { row.sub(&hit.snippet) }
    }
}

/// Text that stays with what follows it.
fn kept(text: String, base: TextStyle) -> StyledBlock {
    let (spans, indent) = (Vec::new(), 0.0);
    StyledBlock::Paragraph(StyledParagraph {
        text,
        spans,
        base,
        muted: INK2,
        indent,
        inert: true,
        keep: true,
        bullet: false,
    })
}

fn article_blocks(article: &Article, t: &Theme) -> Vec<StyledBlock> {
    let gap = || StyledBlock::Gap(t.dictionary_body.line_height() / 2.0);
    let quiet = |text: String| plain(text, t.small.gray(INK2), true);
    let mut blocks = vec![plain(article.title.clone(), t.dictionary_headword, true)];
    if let Some(from) = &article.redirected_from {
        blocks.push(quiet(format!("Redirected from “{from}”")));
    }
    let mut under_heading = false;
    for block in &article.blocks {
        if !under_heading {
            blocks.push(gap());
        }
        under_heading = matches!(block, Block::Heading { .. });
        blocks.push(match block {
            Block::Heading { level, text } => {
                let size = if *level == 2 { t.pt(10.0) } else { t.dictionary_body.size };
                kept(text.clone(), t.dictionary_body.size(size).weight(Weight::SemiBold))
            }
            Block::Paragraph(text) => plain(text.clone(), t.dictionary_body, true),
        });
    }
    if article.blocks.is_empty() {
        blocks.extend([gap(), plain("This article has no text to show.".into(), t.body, true)]);
    }
    let space = t.dictionary_body.line_height() / 2.0;
    blocks.push(StyledBlock::Rule { space, thickness: t.hairline, gray: LINE });
    blocks.push(kept(article.readable_url(), t.small.gray(INK2)));
    blocks.push(quiet(format!("Wikipedia · {} · CC BY-SA 4.0", article.edition.name())));
    blocks
}
