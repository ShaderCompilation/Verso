//! Drawing: the canvas, text and icons.

mod canvas;
mod icons;
mod text;

#[cfg(feature = "desktop")]
pub use canvas::save_gray_png;
pub use canvas::{Canvas, luma};
pub use icons::{Icon, IconRenderer};
pub use text::styled::{SpanStyle, StyledBlock, StyledPagination, StyledParagraph, StyledSpan};
pub use text::{Family, HAlign, TextRenderer, TextStyle, Weight};
