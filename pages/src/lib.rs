//! koubou-pages: multi-page layout engine (.kpages) — frames, flowed text,
//! image placement, and a hand-rolled PDF exporter, all driven by a JSON
//! command protocol (`pg.*`) mirroring `composer::Session`.
//!
//! - [`model`]: `PagesDoc` / `Page` / `MasterPage` / `Frame` serde model
//! - [`session`]: `PgSession` — `dispatch(id, &Value) -> Result<Value>`
//! - [`pdf`]: minimal correct PDF 1.4 writer (FlateDecode streams, base-14
//!   Helvetica, DCTDecode JPEG passthrough, q/Q rotation, `W n` clipping)
//! - [`raster`]: `render_page` PNG previews (rotated bilinear blits)
//! - [`text`]: fontdb/fontdue resolution + in-frame word wrap

pub mod flow;
pub mod model;
pub mod pdf;
pub mod raster;
pub mod session;
pub mod text;

pub use model::{
    Frame, FrameKind, FrameTarget, ImageFit, Margins, MasterPage, Page, PagesDoc, Stroke,
    TextAlign, TextStyle,
};
pub use session::{command_specs, PgSession};
