//! Drawing: the canvas, text and icons.

mod canvas;
mod icons;
mod text;

#[cfg(test)]
pub use canvas::Color;
#[cfg(any(feature = "kindle", test))]
pub use canvas::RotateScratch;
pub use canvas::{Canvas, ColorMode, highlight_gray, luma};
#[cfg(feature = "desktop")]
pub use canvas::{save_gray_png, save_rgba_png};
pub use icons::{Icon, IconRenderer};
pub use text::styled::{SpanStyle, StyledBlock, StyledPagination, StyledParagraph, StyledSpan, StyledWordHit};
pub use text::{Family, HAlign, TextRenderer, TextStyle, Weight};
