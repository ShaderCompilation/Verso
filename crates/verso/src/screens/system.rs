//! Small system controls shared by the library and USB notification.
use crate::geom::Rect;
use crate::gfx::HAlign;
use crate::screens::{Action, EventCx, Screen};
use crate::store::{SleepScreen, Store};
use crate::ui::gesture::Gesture;
use crate::ui::theme::Theme;
use crate::ui::widget::DrawCx;
use crate::ui::widgets::{Button, ButtonKind, Segmented, Sheet, TopBar};

pub struct SystemSettings {
    top: TopBar,
    sleep: Segmented,
    store: Store,
    content: Rect,
}
impl SystemSettings {
    pub fn new(t: &Theme, screen: Rect, store: Store) -> Self {
        let mut top = TopBar::new("Settings", true);
        top.show_battery = true;
        top.place(screen, t);
        let content = screen.split_top(t.top_bar).1.inset(t.margin);
        let mut sleep = Segmented::texts(
            &["Book cover", "Amazon"],
            usize::from(store.settings().sleep_screen == SleepScreen::Amazon),
        );
        sleep.place(Rect::new(content.x, content.y + t.mm(10.0), content.w, t.button_h));
        Self { top, sleep, store, content }
    }
}
impl Screen for SystemSettings {
    fn draw(&mut self, cx: &mut DrawCx) {
        self.top.draw(cx);
        let t = cx.theme;
        cx.section_label("Sleep screen", Rect { h: t.mm(8.0), ..self.content });
        self.sleep.draw(cx);
        cx.paragraph(
            "Show the current book’s cover while asleep. When no cover is available, use Amazon’s sleep screen.",
            t.body,
            Rect::new(self.content.x, self.sleep.rect().bottom() + t.mm(6.0), self.content.w, t.mm(20.0)),
            HAlign::Left,
        );
    }
    fn on_gesture(&mut self, g: Gesture, cx: &mut EventCx) -> Action {
        cx.redraw();
        if self.top.on_gesture(&g) {
            return Action::Pop;
        }
        if let Some(selected) = self.sleep.on_gesture(&g) {
            let mut settings = self.store.settings();
            settings.sleep_screen = if selected == 0 { SleepScreen::Cover } else { SleepScreen::Amazon };
            if let Err(e) = self.store.save_settings(&settings) {
                log::error!("saving sleep screen: {e:#}");
                self.sleep.selected = usize::from(self.store.settings().sleep_screen == SleepScreen::Amazon);
                cx.toast("Couldn't save sleep screen", None);
            }
        }
        Action::None
    }
}

pub struct UsbPrompt {
    sheet: Sheet,
    exit: Button,
    stay: Button,
    text: Rect,
}
impl UsbPrompt {
    pub fn new(t: &Theme, screen: Rect) -> Self {
        let sheet = Sheet::open("USB connected", t.mm(57.0).min(screen.h), screen, t);
        let content = sheet.content(t);
        let mut exit = Button::new("Exit to connect to computer", ButtonKind::Primary);
        exit.place(Rect::new(content.x, screen.bottom() - t.mm(24.0), content.w, t.button_h), t);
        let mut stay = Button::new("Keep reading", ButtonKind::Secondary);
        stay.place(Rect::new(content.x, screen.bottom() - t.mm(12.0), content.w, t.button_h), t);
        Self { text: Rect { h: t.mm(15.0), ..content }, sheet, exit, stay }
    }
}
impl Screen for UsbPrompt {
    fn draw(&mut self, cx: &mut DrawCx) {
        self.sheet.draw(cx);
        cx.paragraph(
            "Verso keeps your books available while charging. Exit to transfer books from your computer.",
            cx.theme.small,
            self.text,
            HAlign::Left,
        );
        self.exit.draw(cx);
        self.stay.draw(cx);
    }
    fn on_gesture(&mut self, g: Gesture, cx: &mut EventCx) -> Action {
        cx.redraw();
        if self.exit.on_gesture(&g) {
            Action::Exit
        } else if self.stay.on_gesture(&g) || self.sheet.on_gesture(&g) {
            Action::Pop
        } else {
            Action::None
        }
    }
}
