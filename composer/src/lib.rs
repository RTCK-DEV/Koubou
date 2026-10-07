//! koubou-composer: layered document engine on top of koubou-core.
//!
//! A `.koubou` document is a JSON layer stack: Develop (live RAW), Raster,
//! Adjustment (develop pipeline on the composite below), Fill, Shape, Text,
//! Group. Compositing follows the PDF/W3C blend-mode spec in sRGB space.

pub mod blend;
pub mod capi;
pub mod commands;
pub mod composite;
pub mod doc;
pub mod psd;
pub mod shape;
pub mod specs;
pub mod text;

pub use blend::BlendMode;
pub use composite::Composer;
pub use doc::{Document, Fill, Layer, LayerKind, Mask, Shape, Stroke, TextAlign, TextContent};
