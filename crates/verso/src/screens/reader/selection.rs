//! Temporary selection and touch capture. At most one engine request is in
//! flight; movement while waiting replaces a single queued focus point.
use crate::document::{
    Selection, SelectionQuery, TextRange,
    thread::{Command, SelectionRequest, Worker},
};
use crate::geom::{Point, Rect};
use crate::screens::EventCx;
use crate::ui::{gesture::Gesture, widget::DrawCx};

#[derive(Default)]
pub(super) struct SelectionState {
    pub selected: Option<Selection>,
    pub extending: bool,
    needs_quote: bool,
    pending: Option<SelectionRequest>,
    next_id: u64,
    anchor: Option<TextRange>,
    queued: Option<Point>,
    captured: bool,
    dragging: bool,
    moved: bool,
    offset: Point,
    released: bool,
}

impl SelectionState {
    pub fn clear(&mut self) {
        self.extending = false;
        self.needs_quote = false;
        self.selected = None;
        self.pending = None;
        self.anchor = None;
        self.queued = None;
        self.dragging = false;
        self.released = false;
    }

    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    pub fn take_released(&mut self) -> Option<String> {
        if !self.released || self.busy() {
            return None;
        }
        self.released = false;
        self.selected.as_ref().map(|s| s.text.clone())
    }

    pub fn adjusting(&self, g: Gesture, theme: &crate::ui::theme::Theme) -> bool {
        if self.captured {
            return !matches!(g, Gesture::Down(_));
        }
        matches!(g, Gesture::Down(p) if self.handles(theme.mm(1.2).max(2)).is_some_and(|handles|
            handles.iter().any(|(center, _)| center.distance(p) <= theme.mm(4.0) as f32)))
    }

    fn finish(&mut self, context: SelectionRequest, worker: &Worker) {
        if self.released
            && !self.busy()
            && self.needs_quote
            && let Some(s) = &self.selected
        {
            let range = s.range.clone();
            self.needs_quote = false;
            self.request(context, SelectionQuery::Range(range), worker);
        }
    }
    pub fn extend(&mut self) {
        self.extending = true;
        self.released = false;
    }
    pub fn page(&mut self, context: SelectionRequest, worker: &Worker) {
        if !self.extending {
            self.clear();
            return;
        }
        self.pending = None;
        self.queued = None;
        self.released = false;
        if let Some(s) = &mut self.selected {
            s.rects.clear();
            s.start_visible = false;
            s.end_visible = false;
            let range = s.range.clone();
            self.request(context, SelectionQuery::Geometry(range), worker);
        }
    }
    pub fn sleep(&mut self) {
        self.clear();
        self.captured = false;
    }

    fn handles(&self, radius: i32) -> Option<[(Point, Point); 2]> {
        let selection = self.selected.as_ref()?;
        let first = selection.rects.first()?;
        let last = selection.rects.last()?;
        Some([
            (
                if selection.start_visible {
                    Point::new(first.x, first.y - radius)
                } else {
                    Point::new(-10000, -10000)
                },
                Point::new(first.x + 1, first.center().y),
            ),
            (
                if selection.end_visible {
                    Point::new(last.right(), last.bottom() + radius)
                } else {
                    Point::new(-10000, -10000)
                },
                Point::new(last.right() - 1, last.center().y),
            ),
        ])
    }

    pub fn draw(&self, cx: &mut DrawCx) {
        let Some(selection) = &self.selected else { return };
        for rect in &selection.rects {
            cx.canvas.invert_rect(*rect);
        }
        let radius = cx.theme.mm(1.2).max(2);
        if let Some(handles) = self.handles(radius) {
            for (center, target) in handles {
                if center.x < 0 {
                    continue;
                }
                cx.fill(Rect::new(center.x - 1, target.y.min(center.y), 3, (center.y - target.y).abs()), 0);
                cx.canvas.fill_circle(center.x as f32, center.y as f32, radius as f32, 0);
            }
        }
    }

    fn request(&mut self, mut context: SelectionRequest, query: SelectionQuery, worker: &Worker) {
        self.next_id += 1;
        context.id = self.next_id;
        self.pending = Some(context);
        worker.send(Command::Select { request: context, query });
    }

    fn focus(&mut self, point: Point, context: SelectionRequest, worker: &Worker) {
        self.needs_quote = true;
        let focus = Point::new(point.x - self.offset.x, point.y - self.offset.y);
        if self.pending.is_some() {
            self.queued = Some(focus);
        } else if let Some(anchor) = self.anchor.clone() {
            self.request(context, SelectionQuery::Extend { anchor, focus }, worker);
        }
    }

    /// Returns true for every event in a touch owned by selection, including
    /// touches dismissing a selection and blank-area long presses.
    pub fn on_gesture(&mut self, g: Gesture, context: SelectionRequest, worker: &Worker, cx: &mut EventCx) -> bool {
        // A timer missed until finger-up can deliver LongPress without End.
        // Every fresh Down still starts a fresh contact.
        if matches!(g, Gesture::Down(_)) {
            self.captured = false;
            self.dragging = false;
        }
        match g {
            Gesture::Down(p) if self.selected.is_some() || self.pending.is_some() => {
                self.captured = true;
                self.dragging = false;
                self.moved = false;
                let radius = cx.theme.mm(1.2).max(2);
                let handle = self.handles(radius).and_then(|handles| {
                    handles
                        .into_iter()
                        .enumerate()
                        .filter(|(_, (center, _))| center.distance(p) <= cx.theme.mm(4.0) as f32)
                        .min_by(|(_, (a, _)), (_, (b, _))| a.distance(p).total_cmp(&b.distance(p)))
                });
                if let Some((index, (_, target))) = handle {
                    self.released = false;
                    let range = &self.selected.as_ref().unwrap().range;
                    let fixed = if index == 0 { &range.end } else { &range.start }.clone();
                    self.anchor = Some(TextRange { start: fixed.clone(), end: fixed });
                    self.queued = None;
                    self.offset = Point::new(p.x - target.x, p.y - target.y);
                    self.dragging = true;
                } else if self.extending {
                    self.dragging = true;
                    self.offset = Point::default();
                    self.focus(p, context, worker);
                } else {
                    self.clear();
                    cx.redraw();
                }
                true
            }
            Gesture::LongPress(p) if !self.captured => {
                self.clear();
                self.captured = true;
                self.dragging = true;
                self.moved = false;
                self.offset = Point::default();
                self.request(context, SelectionQuery::Word(p), worker);
                true
            }
            Gesture::Move(p) if self.captured => {
                if self.dragging {
                    self.moved = true;
                    self.focus(p, context, worker);
                }
                true
            }
            _ if self.captured => {
                if g.ends_touch() {
                    self.released = self.dragging;
                    if self.dragging && (self.moved || matches!(g, Gesture::Swipe { .. })) {
                        self.focus(g.pos(), context, worker);
                    }
                    self.dragging = false;
                    self.captured = false;
                    self.finish(context, worker);
                }
                true
            }
            _ => false,
        }
    }

    pub fn receive(
        &mut self,
        request: SelectionRequest,
        result: Result<Option<Selection>, String>,
        context: Option<SelectionRequest>,
        worker: &Worker,
        cx: &mut EventCx,
    ) {
        if self.pending != Some(request)
            || context.is_none_or(|c| c.generation != request.generation || c.page != request.page)
        {
            return;
        }
        self.pending = None;
        match result {
            Ok(Some(mut selection)) => {
                if selection.text.is_empty() {
                    selection.text = self.selected.as_ref().map(|s| s.text.clone()).unwrap_or_default();
                }
                // The initial word remains fixed while extending in either direction.
                if self.anchor.is_none() {
                    self.anchor = Some(selection.range.clone());
                }
                self.selected = Some(selection);
                cx.redraw();
            }
            Ok(None) => {} // Blank space leaves the last valid selection intact.
            Err(message) => {
                log::warn!("selection: {message}");
                cx.toast("Couldn't select text", None);
            }
        }
        if let Some(focus) = self.queued.take()
            && let Some(anchor) = self.anchor.clone()
        {
            self.request(request, SelectionQuery::Extend { anchor, focus }, worker);
        }
        self.finish(request, worker);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Position;
    use crate::ui::{gesture::Direction, theme::Theme};

    fn with_context(
        test: impl FnOnce(&mut SelectionState, &Worker, &std::sync::mpsc::Receiver<Command>, &mut EventCx),
    ) {
        let (worker, commands) = Worker::recording_stub();
        let mut state = SelectionState::default();
        let theme = Theme::for_dpi(150);
        let mut cx = EventCx {
            theme: &theme,
            screen: Rect::new(0, 0, 600, 800),
            now: std::time::Instant::now(),
            frontlight: &mut None,
            light_request: &mut None,
            toast: &mut None,
            forced: &mut Vec::new(),
            needs_redraw: &mut false,
            battery: None,
        };
        test(&mut state, &worker, &commands, &mut cx);
    }
    const CONTEXT: SelectionRequest = SelectionRequest { generation: 7, page: 3, id: 0 };
    fn selected(text: &str) -> Selection {
        Selection {
            range: TextRange { start: Position("start".into()), end: Position("end".into()) },
            text: text.into(),
            rects: vec![Rect::new(100, 100, 200, 30)],
            start_page: 0,
            start_visible: true,
            end_visible: true,
        }
    }
    fn next(commands: &std::sync::mpsc::Receiver<Command>) -> (SelectionRequest, SelectionQuery) {
        match commands.try_recv().unwrap() {
            Command::Select { request, query } => (request, query),
            _ => panic!("expected selection request"),
        }
    }

    #[test]
    fn movement_before_word_reply_is_bounded_and_release_keeps_final_focus() {
        with_context(|state, worker, commands, cx| {
            assert!(state.on_gesture(Gesture::LongPress(Point::new(120, 110)), CONTEXT, worker, cx));
            let (word, _) = next(commands);
            for x in 121..300 {
                assert!(state.on_gesture(Gesture::Move(Point::new(x, 110)), CONTEXT, worker, cx));
            }
            assert!(commands.try_recv().is_err(), "only one request can be in flight");
            assert!(state.on_gesture(Gesture::End(Point::new(310, 110)), CONTEXT, worker, cx));
            state.receive(word, Ok(Some(selected("word"))), Some(CONTEXT), worker, cx);
            let (extension, query) = next(commands);
            assert!(matches!(query, SelectionQuery::Extend { focus: Point { x: 310, y: 110 }, .. }));
            assert_eq!((extension.generation, extension.page), (7, 3));
            assert!(extension.id > word.id);
            state.receive(word, Ok(Some(selected("obsolete"))), Some(CONTEXT), worker, cx);
            assert_eq!(state.selected.as_ref().unwrap().text, "word");
            state.receive(extension, Ok(Some(selected("passage"))), Some(CONTEXT), worker, cx);
            assert_eq!(state.selected.as_ref().unwrap().text, "passage");
            let (quote, query) = next(commands);
            assert!(matches!(query, SelectionQuery::Range(_)));
            state.receive(quote, Ok(Some(selected("passage"))), Some(CONTEXT), worker, cx);
            assert!(!state.busy());
        });
    }

    #[test]
    fn handles_keep_opposite_endpoint_and_consume_swipes_and_dismissal() {
        with_context(|state, worker, commands, cx| {
            state.selected = Some(selected("passage"));
            let end = state.handles(cx.theme.mm(1.2)).unwrap()[1].0;
            assert!(state.on_gesture(Gesture::Down(end), CONTEXT, worker, cx));
            let to = Point::new(end.x - 250, end.y);
            assert!(state.on_gesture(Gesture::Move(to), CONTEXT, worker, cx));
            let (request, query) = next(commands);
            match query {
                SelectionQuery::Extend { anchor, focus } => {
                    assert_eq!(anchor.start, Position("start".into()));
                    assert_eq!(anchor.end, anchor.start);
                    assert_eq!(focus, Point::new(49, 115));
                }
                _ => panic!("expected handle adjustment"),
            }
            assert!(state.on_gesture(Gesture::Swipe { dir: Direction::Left, from: end, to }, CONTEXT, worker, cx));
            state.receive(request, Ok(None), Some(CONTEXT), worker, cx);
            assert_eq!(state.selected.as_ref().unwrap().text, "passage", "blank drag preserves selection");
            let outside = Point::new(590, 600);
            assert!(state.on_gesture(Gesture::Down(outside), CONTEXT, worker, cx));
            assert!(state.selected.is_none());
            assert!(state.on_gesture(Gesture::Tap(outside), CONTEXT, worker, cx));
            assert!(!state.on_gesture(Gesture::Down(outside), CONTEXT, worker, cx));
            assert!(!state.on_gesture(Gesture::Tap(outside), CONTEXT, worker, cx));
        });
    }

    #[test]
    fn obsolete_book_layout_page_request_and_sleep_replies_are_ignored() {
        with_context(|state, worker, commands, cx| {
            let press = Gesture::LongPress(Point::new(120, 110));
            state.on_gesture(press, CONTEXT, worker, cx);
            let (request, _) = next(commands);
            for context in [
                None,
                Some(SelectionRequest { page: 4, ..CONTEXT }),
                Some(SelectionRequest { generation: 8, ..CONTEXT }),
            ] {
                state.receive(request, Ok(Some(selected("wrong"))), context, worker, cx);
                assert!(state.selected.is_none());
            }
            state.sleep();
            state.receive(request, Ok(Some(selected("slept"))), Some(CONTEXT), worker, cx);
            assert!(state.selected.is_none());
            state.on_gesture(press, CONTEXT, worker, cx);
            let (new, _) = next(commands);
            assert!(new.id > request.id, "IDs must not be reused after sleep");
            state.receive(request, Ok(Some(selected("old"))), Some(CONTEXT), worker, cx);
            assert!(state.selected.is_none());
            state.receive(new, Ok(Some(selected("new"))), Some(CONTEXT), worker, cx);
            assert_eq!(state.selected.as_ref().unwrap().text, "new");
            state.clear(); // Same path used by typography, displayed-page changes, and close.
            state.receive(new, Ok(Some(selected("late"))), Some(CONTEXT), worker, cx);
            assert!(state.selected.is_none());
        });
    }

    #[test]
    fn blank_long_press_with_a_missed_release_does_not_capture_the_next_touch() {
        with_context(|state, worker, commands, cx| {
            let p = Point::new(590, 10);
            state.on_gesture(Gesture::LongPress(p), CONTEXT, worker, cx);
            let (request, _) = next(commands);
            state.receive(request, Ok(None), Some(CONTEXT), worker, cx);
            assert!(!state.on_gesture(Gesture::Down(p), CONTEXT, worker, cx));
            assert!(!state.on_gesture(Gesture::Tap(p), CONTEXT, worker, cx));
        });
    }

    #[test]
    fn release_waits_for_the_final_reply_and_is_consumed_once() {
        with_context(|state, worker, commands, cx| {
            let p = Point::new(120, 110);
            state.on_gesture(Gesture::LongPress(p), CONTEXT, worker, cx);
            let (request, _) = next(commands);
            assert!(state.take_released().is_none());
            state.on_gesture(Gesture::End(p), CONTEXT, worker, cx);
            assert!(state.take_released().is_none());
            state.receive(request, Ok(Some(selected("word"))), Some(CONTEXT), worker, cx);
            assert_eq!(state.take_released().as_deref(), Some("word"));
            assert!(state.take_released().is_none());
            state.sleep();
            assert!(state.take_released().is_none());
        });
    }
    #[test]
    fn extending_pages_preserves_endpoints_hides_false_handles_and_rejects_old_replies() {
        with_context(|state, worker, commands, cx| {
            let initial = selected("passage");
            state.selected = Some(initial.clone());
            state.anchor = Some(initial.range.clone());
            state.extend();
            let next_page = SelectionRequest { page: 4, ..CONTEXT };
            state.page(next_page, worker);
            let (geometry, query) = next(commands);
            assert!(matches!(query,SelectionQuery::Geometry(ref r) if *r==initial.range));
            assert_eq!(state.selected.as_ref().unwrap().range, initial.range);
            assert!(state.handles(3).is_none());
            let mut visible = initial.clone();
            visible.start_visible = false;
            visible.text.clear();
            state.receive(geometry, Ok(Some(visible)), Some(next_page), worker, cx);
            assert_eq!(state.selected.as_ref().unwrap().text, "passage");
            assert!(state.handles(3).unwrap()[0].0.x < 0);
            state.on_gesture(Gesture::Down(Point::new(350, 110)), next_page, worker, cx);
            let (request, query) = next(commands);
            assert!(matches!(query,SelectionQuery::Extend{anchor,..} if anchor==initial.range));
            state.receive(request, Ok(Some(selected("wrong"))), Some(CONTEXT), worker, cx);
            assert_eq!(state.selected.as_ref().unwrap().text, "passage");
            state.sleep();
            assert!(!state.extending);
            state.receive(request, Ok(Some(selected("late"))), Some(next_page), worker, cx);
            assert!(state.selected.is_none());
        });
    }
}
