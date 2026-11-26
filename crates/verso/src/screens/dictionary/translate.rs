//! The Translate tab for one word or passage: its translation, and what keeps
//! it from being asked for.
use super::{
    plain,
    tabs::{Languages, Slot, message},
};
use crate::{
    dictionary::languages::language_name,
    gfx::StyledBlock,
    online::{
        Answer, Error, Reply, Request, Worker,
        translate::{Provider, Translation, check, effective_provider, languages, same_language},
    },
    ui::theme::{INK2, Theme},
};

#[derive(Default)]
pub struct Translate(Slot<Translation>);

impl Translate {
    pub fn is_unasked(&self) -> bool {
        matches!(self.0, Slot::Unasked)
    }

    pub fn waiting(&self) -> bool {
        self.0.waiting()
    }

    /// Asks unless it already did or no language to translate into is chosen.
    /// What can't be translated is refused here, before a thread starts.
    pub fn ask(&mut self, worker: &Worker, languages: &Languages, text: &str) {
        let Some(target) = languages.target.filter(|_| self.is_unasked()) else { return };
        self.0 = match check(text, languages.source, target) {
            Ok(()) => {
                let source = languages.source.map(Into::into);
                Slot::Waiting(worker.request(Request::Translate {
                    text: text.into(),
                    source,
                    target: target.into(),
                    provider: languages.provider,
                    api_key: languages.deepl_api_key.clone(),
                }))
            }
            Err(e) => Slot::Done(Err(e)),
        };
    }

    /// Takes in a reply; false when it isn't waited for.
    pub fn receive(&mut self, reply: Reply) -> bool {
        if !self.0.waits_for(reply.id) {
            return false;
        }
        self.0 = Slot::Done(match reply.result {
            Ok(Answer::Translation(translation)) => Ok(translation),
            Ok(_) => Err(Error::Unreadable),
            Err(e) => Err(e),
        });
        true
    }

    pub fn error(&self) -> Option<&Error> {
        self.0.error()
    }

    pub fn finished(self) -> Self {
        Self(self.0.finished())
    }

    /// The language the service found the text to be in.
    pub fn detected(&self) -> Option<&str> {
        if let Slot::Done(Ok(translation)) = &self.0 { translation.detected.as_deref() } else { None }
    }

    /// The text is already in the language to translate into, so another one
    /// is needed.
    pub fn needs_another_language(&self, languages: &Languages) -> bool {
        self.already_in(languages).is_some() || self.error() == Some(&Error::SameLanguage)
    }

    fn already_in(&self, languages: &Languages) -> Option<&str> {
        let target = languages.target.filter(|_| languages.source.is_none())?;
        self.detected().filter(|detected| same_language(detected, target))
    }

    /// The source button's label.
    pub fn source_label(&self, languages: &Languages) -> String {
        match (languages.source, self.detected()) {
            (Some(source), _) => language_name(source).into(),
            (None, Some(detected)) => format!("Auto ({})", language_name(detected)),
            (None, None) => "Auto".into(),
        }
    }

    /// What shows under the tab's row once a language is chosen.
    pub fn content(&self, languages: &Languages, t: &Theme) -> Vec<StyledBlock> {
        let line = |s: &str| vec![plain(s.into(), t.body, true)];
        if let Some(language) = self.already_in(languages) {
            let title = format!("This is already {}", language_name(language));
            return message(&title, "It’s in the language you translate into. Choose another one for it.", t);
        }
        match &self.0 {
            Slot::Unasked => line("Edit the words above to translate them."),
            Slot::Waiting(_) => line("Translating…"),
            Slot::Done(Ok(translation)) => {
                let mut blocks = Vec::new();
                for text in translation.text.lines().map(str::trim).filter(|l| !l.is_empty()) {
                    blocks.push(plain(text.into(), t.dictionary_body, true));
                    blocks.push(StyledBlock::Gap(t.dictionary_body.line_height() / 2.0));
                }
                blocks.push(plain(translation.provider.service_name().into(), t.small.gray(INK2), true));
                blocks
            }
            Slot::Done(Err(e)) => {
                let service = languages
                    .target
                    .map_or(Provider::Google, |target| {
                        effective_provider(
                            languages.provider,
                            languages.deepl_api_key.as_deref(),
                            languages.source,
                            target,
                        )
                    })
                    .service_name();
                message(&e.title(service), &e.plain(), t)
            }
        }
    }
}

/// The languages to translate into, the likely ones first: the one in use,
/// English and the book's.
pub fn targets(l: &Languages) -> Vec<&'static str> {
    likely_first([l.target, Some("en"), l.written])
}

/// The languages to translate from, the likely ones first. None detects it.
pub fn sources(l: &Languages, detected: Option<&str>) -> Vec<Option<&'static str>> {
    let detected = detected.and_then(crate::online::translate::language);
    std::iter::once(None).chain(likely_first([l.source, detected, l.written]).into_iter().map(Some)).collect()
}

fn likely_first(likely: [Option<&'static str>; 3]) -> Vec<&'static str> {
    let mut all = Vec::new();
    for language in likely.into_iter().flatten().chain(languages()) {
        if !all.contains(&language) {
            all.push(language);
        }
    }
    all
}
