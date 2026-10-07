//! Document model: a `.koubou` file = one JSON document.
//!
//! Coordinate space: document pixels, origin top-left. Layers rasterize into
//! their own pixel space and are placed at `x`,`y` (can be negative) with an
//! optional uniform `scale`. Everything serializes camelCase for JSON and the
//! control channel.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use koubou_core::Recipe;

use crate::blend::BlendMode;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Document {
    pub version: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// canvas backdrop shown behind transparent areas: [r,g,b,a] 0..1 sRGB
    #[serde(default)]
    pub backdrop: [f32; 4],
    /// bottom -> top
    #[serde(default)]
    pub layers: Vec<Layer>,
    /// next layer id (monotonic, survives undo/redo serialization)
    #[serde(default = "one")]
    pub next_id: u64,
}

fn one() -> u64 {
    1
}

impl Document {
    pub fn new(name: impl Into<String>, width: u32, height: u32) -> Document {
        Document {
            version: 1,
            name: name.into(),
            width,
            height,
            backdrop: [0.0; 4],
            layers: Vec::new(),
            next_id: 1,
        }
    }

    /// doc sized to a photo: base layer = Develop source at 0,0.
    pub fn from_photo(path: &Path, w: u32, h: u32) -> Document {
        let mut d = Document::new(
            path.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Untitled".into()),
            w,
            h,
        );
        d.add_layer(Layer::develop(path));
        d
    }

    pub fn add_layer(&mut self, mut l: Layer) -> u64 {
        l.id = self.next_id;
        self.next_id += 1;
        let id = l.id;
        self.layers.push(l);
        id
    }

    /// add a layer at a specific stack index (0 = bottom); id is assigned
    /// fresh and nested group children get fresh ids too so they never
    /// collide with the compositor's (id, gen) cache.
    pub fn add_layer_at(&mut self, mut l: Layer, at: usize) -> u64 {
        Self::reassign_ids(&mut l, &mut self.next_id);
        let id = l.id;
        self.layers.insert(at.min(self.layers.len()), l);
        id
    }

    fn reassign_ids(l: &mut Layer, next: &mut u64) {
        l.id = *next;
        *next += 1;
        l.gen += 1;
        if let LayerKind::Group { children } = &mut l.kind {
            for c in children.iter_mut() {
                Self::reassign_ids(c, next);
            }
        }
    }

    /// invalidate every layer's cached pixels (canvas resize / crop —
    /// fills, shapes and masks all rasterize in doc space)
    pub fn bump_all_gens(&mut self) {
        fn bump(ls: &mut [Layer]) {
            for l in ls.iter_mut() {
                l.gen += 1;
                if let LayerKind::Group { children } = &mut l.kind {
                    bump(children);
                }
            }
        }
        bump(&mut self.layers);
    }

    pub fn layer(&self, id: u64) -> Option<&Layer> {
        self.layers.iter().find(|l| l.id == id)
    }

    pub fn layer_mut(&mut self, id: u64) -> Option<&mut Layer> {
        self.layers.iter_mut().find(|l| l.id == id)
    }

    /// index of a layer (0 = bottom). usize::MAX when absent.
    pub fn index_of(&self, id: u64) -> Option<usize> {
        self.layers.iter().position(|l| l.id == id)
    }

    pub fn remove_layer(&mut self, id: u64) -> Option<Layer> {
        let i = self.index_of(id)?;
        Some(self.layers.remove(i))
    }

    /// move layer to a new stack index (0 = bottom)
    pub fn reorder(&mut self, id: u64, to: usize) -> bool {
        let Some(i) = self.index_of(id) else {
            return false;
        };
        let l = self.layers.remove(i);
        let to = to.min(self.layers.len());
        self.layers.insert(to, l);
        true
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }

    pub fn from_json(s: &str) -> Result<Document> {
        serde_json::from_str(s).context("invalid .koubou document JSON")
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, self.to_json()).with_context(|| format!("write {}", path.display()))
    }

    pub fn load(path: &Path) -> Result<Document> {
        let s =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        Document::from_json(&s)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Layer {
    #[serde(default)]
    pub id: u64,
    pub name: String,
    #[serde(default = "one_f")]
    pub opacity: f32,
    #[serde(default)]
    pub visible: bool,
    #[serde(default)]
    pub blend: BlendMode,
    /// layer-space offset in document pixels
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
    /// uniform scale applied after rasterization (1 = natural size)
    #[serde(default = "one_f")]
    pub scale: f32,
    #[serde(default)]
    pub mask: Option<Mask>,
    /// content generation — bumped on every content edit for cache invalidation
    #[serde(default)]
    pub gen: u64,
    #[serde(flatten)]
    pub kind: LayerKind,
}

fn one_f() -> f32 {
    1.0
}

impl Layer {
    pub fn base(name: impl Into<String>, kind: LayerKind) -> Layer {
        Layer {
            id: 0,
            name: name.into(),
            opacity: 1.0,
            visible: true,
            blend: BlendMode::Normal,
            x: 0,
            y: 0,
            scale: 1.0,
            mask: None,
            gen: 0,
            kind,
        }
    }

    pub fn develop(path: &Path) -> Layer {
        Layer::base(
            path.file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Photo".into()),
            LayerKind::Develop {
                path: path.to_path_buf(),
                recipe: Recipe::default(),
            },
        )
    }

    pub fn raster(name: impl Into<String>, w: u32, h: u32, rgba: Vec<u8>) -> Layer {
        use base64::Engine as _;
        Layer::base(
            name,
            LayerKind::Raster {
                width: w,
                height: h,
                src: RasterSrc::Embedded {
                    png_b64: base64::engine::general_purpose::STANDARD.encode(&rgba),
                },
            },
        )
    }

    pub fn adjustment(name: impl Into<String>, recipe: Recipe) -> Layer {
        Layer::base(name, LayerKind::Adjustment { recipe })
    }

    pub fn fill(name: impl Into<String>, fill: Fill) -> Layer {
        Layer::base(name, LayerKind::Fill { fill })
    }

    pub fn text(name: impl Into<String>, text: TextContent) -> Layer {
        Layer::base(name, LayerKind::Text { text })
    }

    pub fn shape(name: impl Into<String>, shapes: Vec<Shape>) -> Layer {
        Layer::base(name, LayerKind::Shape { shapes })
    }

    pub fn group(name: impl Into<String>, children: Vec<Layer>) -> Layer {
        Layer::base(name, LayerKind::Group { children })
    }

    pub fn kind_name(&self) -> &'static str {
        match &self.kind {
            LayerKind::Develop { .. } => "develop",
            LayerKind::Raster { .. } => "raster",
            LayerKind::Adjustment { .. } => "adjustment",
            LayerKind::Fill { .. } => "fill",
            LayerKind::Shape { .. } => "shape",
            LayerKind::Text { .. } => "text",
            LayerKind::Group { .. } => "group",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum LayerKind {
    /// a source file (RAW or raster) developed live through the engine
    Develop {
        path: PathBuf,
        recipe: Recipe,
    },
    /// baked pixels
    Raster {
        width: u32,
        height: u32,
        src: RasterSrc,
    },
    /// tone/colour operations applied to the composite of everything below
    Adjustment {
        recipe: Recipe,
    },
    Fill {
        fill: Fill,
    },
    Shape {
        shapes: Vec<Shape>,
    },
    Text {
        text: TextContent,
    },
    /// children composite into an isolated buffer, blended as one layer
    Group {
        children: Vec<Layer>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "src", rename_all = "camelCase")]
pub enum RasterSrc {
    /// base64-encoded RGBA8 (not PNG — raw channel bytes for exactness)
    Embedded { png_b64: String },
    /// external image file, decoded on demand
    File { path: PathBuf },
}

impl RasterSrc {
    pub fn decode(&self) -> Result<(u32, u32, Vec<u8>)> {
        match self {
            RasterSrc::Embedded { png_b64 } => {
                use base64::Engine as _;
                let raw = base64::engine::general_purpose::STANDARD
                    .decode(png_b64)
                    .context("embedded raster: bad base64")?;
                // embedded data is raw rgba8; dimensions come from the layer
                Ok((0, 0, raw))
            }
            RasterSrc::File { path } => {
                let img = image::open(path)
                    .with_context(|| format!("decode raster {}", path.display()))?
                    .to_rgba8();
                let (w, h) = img.dimensions();
                Ok((w, h, img.into_raw()))
            }
        }
    }
}

/// grayscale layer mask in layer pixel space
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mask {
    pub width: u32,
    pub height: u32,
    /// 0..1 coverage per pixel, row-major
    #[serde(with = "b64_f32")]
    pub data: Vec<f32>,
    #[serde(default)]
    pub inverted: bool,
    /// multiplies coverage, 0..1
    #[serde(default = "one_f")]
    pub density: f32,
    /// gaussian-ish feather radius in px (applied lazily)
    #[serde(default)]
    pub feather: f32,
}

impl Mask {
    pub fn full(w: u32, h: u32) -> Mask {
        Mask {
            width: w,
            height: h,
            data: vec![1.0; (w * h) as usize],
            inverted: false,
            density: 1.0,
            feather: 0.0,
        }
    }

    /// coverage at layer-space pixel (x,y) after inversion/density
    pub fn at(&self, x: u32, y: u32) -> f32 {
        if x >= self.width || y >= self.height {
            return 0.0;
        }
        let mut v = self.data[(y * self.width + x) as usize];
        if self.inverted {
            v = 1.0 - v;
        }
        (v * self.density).clamp(0.0, 1.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Fill {
    Solid {
        /// [r,g,b,a] 0..1 sRGB
        color: [f32; 4],
    },
    /// linear gradient across the layer box
    LinearGradient {
        /// [x0,y0,x1,y1] in 0..1 of the layer box
        line: [f32; 4],
        /// [pos, r,g,b,a] stops sorted by pos
        stops: Vec<[f32; 5]>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Shape {
    /// SVG path data (M/L/H/V/C/Q/Z, abs+rel)
    pub d: String,
    #[serde(default)]
    pub fill: Option<[f32; 4]>,
    #[serde(default)]
    pub stroke: Option<Stroke>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stroke {
    pub color: [f32; 4],
    #[serde(default = "one_f")]
    pub width: f32,
    /// SVG dasharray — alternating dash/gap lengths in px
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dash: Option<Vec<f32>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextContent {
    pub text: String,
    /// font family name or absolute .ttf/.otf path; empty = system default
    #[serde(default)]
    pub font: String,
    #[serde(default = "default_size")]
    pub size: f32,
    /// extra letter spacing, px
    #[serde(default)]
    pub tracking: f32,
    /// line height multiplier (1.0 = font natural leading)
    #[serde(default = "default_leading")]
    pub leading: f32,
    #[serde(default)]
    pub align: TextAlign,
    /// [r,g,b,a] 0..1 sRGB
    #[serde(default = "default_color")]
    pub color: [f32; 4],
    /// wrap width in px; 0/None = point text (auto-size)
    #[serde(default)]
    pub wrap_width: f32,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
}

fn default_size() -> f32 {
    48.0
}
fn default_leading() -> f32 {
    1.2
}
fn default_color() -> [f32; 4] {
    [1.0, 1.0, 1.0, 1.0]
}

impl Default for TextContent {
    fn default() -> Self {
        TextContent {
            text: String::new(),
            font: String::new(),
            size: default_size(),
            tracking: 0.0,
            leading: default_leading(),
            align: TextAlign::Left,
            color: default_color(),
            wrap_width: 0.0,
            bold: false,
            italic: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

impl Default for TextAlign {
    fn default() -> Self {
        TextAlign::Left
    }
}

/// f32 vec as base64 for compact JSON masks
mod b64_f32 {
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[f32], s: S) -> Result<S::Ok, S::Error> {
        let bytes: &[u8] = bytemuck::cast_slice(v);
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<f32>, D::Error> {
        let s = String::deserialize(d)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&s)
            .map_err(serde::de::Error::custom)?;
        bytemuck::try_cast_slice::<u8, f32>(&bytes)
            .map(|sl| sl.to_vec())
            .map_err(|_| serde::de::Error::custom("mask data length not a multiple of 4"))
    }
}
