//! The widgets. Each is a plain struct: place it, draw it, feed it gestures.
//! See docs/design.md §5 for how each one looks and refreshes.

pub mod button;
pub mod grid;
pub mod keyboard;
pub mod list;
pub mod pager;
pub mod popup_menu;
pub mod segmented;
pub mod sheet;
pub mod slider;
pub mod stepper;
pub mod text_area;
pub mod text_field;
pub mod toast;
pub mod toggle;
pub mod top_bar;

pub use button::{Button, ButtonKind, IconButton};
pub use grid::{Grid, GridEvent};
pub use list::{List, ListEvent, Row};
pub use pager::{Pager, PagerEvent};
pub use popup_menu::{MenuEvent, PopupMenu};
pub use segmented::{Segment, Segmented};
pub use sheet::Sheet;
pub use slider::{Slider, SliderEvent};
pub use stepper::{SideLabel, Step, Stepper};
pub use toast::Toast;
pub use toggle::Toggle;
pub use top_bar::TopBar;

pub use keyboard::Keyboard;
pub use text_area::TextArea;
pub use text_field::TextField;
