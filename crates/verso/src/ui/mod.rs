//! The UI toolkit: theme, layout, gestures, damage tracking and widgets.
//! How it fits together: docs/design.md §6.

pub mod damage;
pub mod gesture;
#[allow(dead_code)] // the full toolkit; today's screens use only part of it
pub mod layout;
pub mod theme;
pub mod widget;
#[allow(dead_code)] // the full toolkit; today's screens use only part of it
pub mod widgets;
