//! The UI core (ADR-0058): one layout, text and paint path for native game
//! UI and the Roblox-style GUI.
//!
//! A host builds a [`UiTree`] for a frame: plain [`Node`]s with sizes as
//! scale plus offset, anchors, padding, size limits, lists and grids,
//! scrolling, text, images and looks. [`Layout`] places each [`Root`]'s nodes
//! on the screen (or on a surface's canvas) and gives the drawn [`Item`]s in
//! drawing order, and a [`Visit`] per node with the values a host reports
//! back (the absolute position and size, text bounds, a list's content
//! size). [`Painter`] turns items into egui shapes, which the host paints
//! through re_render's UI pass. Text is laid out with egui's fonts and the
//! bundled families ([`fonts`]).

pub mod fonts;
mod layout;
mod paint;
pub mod text;
mod tree;

pub use fonts::{Face, Family};
pub use layout::{Arranged, Item, LaidText, Layout, Root, ScrollVisit, Visit};
pub use paint::{Looks, Painter};
pub use text::TextStyle;
pub use tree::{
    Arrange, Aspect, Axes, Axis, Corner, Dim, Dim2, Gradient, Grid, Image, List, Look, Node,
    NodeId, Padding, Rgba, Scroll, SizeLimits, Sort, Stroke, StrokeMode, UiTree,
};
