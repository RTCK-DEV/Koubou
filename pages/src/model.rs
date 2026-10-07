//! `.kpages` document model — one JSON document, multi-page layout.
//!
//! Coordinate space: points (1/72"), origin top-left of the page, y grows
//! downward (InDesign-style). PDF export flips to PDF's bottom-left space.
//! Everything serializes camelCase for the control channel.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PagesDoc {
    pub version: u32,
    pub name: String,
    /// page size in points
    pub page_w: f32,
    pub page_h: f32,
    /// page margins in points (layout guides, not enforced)
    #[serde(default)]
    pub margins: Margins,
    #[serde(default)]
    pub pages: Vec<Page>,
    #[serde(default)]
    pub masters: Vec<MasterPage>,
    /// next frame id (monotonic across pages and masters)
    #[serde(default = "one")]
    pub next_id: u64,
}

fn one() -> u64 {
    1
}

impl PagesDoc {
    pub fn new(name: impl Into<String>, page_w: f32, page_h: f32) -> PagesDoc {
        PagesDoc {
            version: 1,
            name: name.into(),
            page_w,
            page_h,
            margins: Margins::default(),
            pages: Vec::new(),
            masters: Vec::new(),
            next_id: 1,
        }
    }

    /// append a page; `master` indexes into `masters` when present.
    /// Returns the new page index.
    pub fn add_page(&mut self, master: Option<usize>) -> Result<usize> {
        if let Some(m) = master {
            if m >= self.masters.len() {
                anyhow::bail!("master {m} out of range ({} masters)", self.masters.len());
            }
        }
        self.pages.push(Page {
            frames: Vec::new(),
            master,
        });
        Ok(self.pages.len() - 1)
    }

    pub fn remove_page(&mut self, idx: usize) -> Result<Page> {
        if idx >= self.pages.len() {
            anyhow::bail!("page {idx} out of range ({} pages)", self.pages.len());
        }
        Ok(self.pages.remove(idx))
    }

    pub fn add_master(&mut self, name: impl Into<String>) -> usize {
        self.masters.push(MasterPage {
            name: name.into(),
            frames: Vec::new(),
        });
        self.masters.len() - 1
    }

    /// allocate a frame id and push the frame onto a page or a master.
    /// `target` selects the container.
    pub fn add_frame(&mut self, target: FrameTarget, mut f: Frame) -> Result<u64> {
        f.id = self.next_id;
        self.next_id += 1;
        let id = f.id;
        self.frames_mut(target)?.push(f);
        Ok(id)
    }

    /// frames of a page (mutable), or of a master page
    pub fn frames_mut(&mut self, target: FrameTarget) -> Result<&mut Vec<Frame>> {
        match target {
            FrameTarget::Page(i) => {
                let n = self.pages.len();
                self.pages
                    .get_mut(i)
                    .map(|p| &mut p.frames)
                    .with_context(|| format!("page {i} out of range ({n} pages)"))
            }
            FrameTarget::Master(i) => {
                let n = self.masters.len();
                self.masters
                    .get_mut(i)
                    .map(|m| &mut m.frames)
                    .with_context(|| format!("master {i} out of range ({n} masters)"))
            }
        }
    }

    /// find a frame by id across pages and masters
    pub fn frame_mut(&mut self, id: u64) -> Option<&mut Frame> {
        for p in &mut self.pages {
            if let Some(f) = p.frames.iter_mut().find(|f| f.id == id) {
                return Some(f);
            }
        }
        for m in &mut self.masters {
            if let Some(f) = m.frames.iter_mut().find(|f| f.id == id) {
                return Some(f);
            }
        }
        None
    }

    pub fn remove_frame(&mut self, id: u64) -> Option<Frame> {
        for p in &mut self.pages {
            if let Some(i) = p.frames.iter().position(|f| f.id == id) {
                return Some(p.frames.remove(i));
            }
        }
        for m in &mut self.masters {
            if let Some(i) = m.frames.iter().position(|f| f.id == id) {
                return Some(m.frames.remove(i));
            }
        }
        None
    }

    /// frames drawn on a page, bottom to top: the master's frames first
    /// (master items always sit behind page items, InDesign-style), then the
    /// page's own; each list sorted by z (stable, so equal z keeps document
    /// order).
    pub fn resolved_frames(&self, page_idx: usize) -> Result<Vec<Frame>> {
        let p = self.pages.get(page_idx).with_context(|| {
            format!("page {page_idx} out of range ({} pages)", self.pages.len())
        })?;
        let mut frames: Vec<Frame> = Vec::new();
        if let Some(mi) = p.master {
            let m = self
                .masters
                .get(mi)
                .with_context(|| format!("master {mi} out of range"))?;
            let mut mf: Vec<Frame> = m.frames.clone();
            mf.sort_by_key(|f| f.z);
            frames.extend(mf);
        }
        let mut own: Vec<Frame> = p.frames.clone();
        own.sort_by_key(|f| f.z);
        frames.extend(own);
        Ok(frames)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }

    pub fn from_json(s: &str) -> Result<PagesDoc> {
        serde_json::from_str(s).context("invalid .kpages document JSON")
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, self.to_json()).with_context(|| format!("write {}", path.display()))
    }

    pub fn load(path: &Path) -> Result<PagesDoc> {
        let s =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        PagesDoc::from_json(&s)
    }
}

/// which container a frame belongs to
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameTarget {
    Page(usize),
    Master(usize),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    #[serde(default)]
    pub frames: Vec<Frame>,
    /// index into `masters`, applied beneath this page's frames
    #[serde(default)]
    pub master: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MasterPage {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub frames: Vec<Frame>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Margins {
    #[serde(default = "default_margin")]
    pub top: f32,
    #[serde(default = "default_margin")]
    pub right: f32,
    #[serde(default = "default_margin")]
    pub bottom: f32,
    #[serde(default = "default_margin")]
    pub left: f32,
}

fn default_margin() -> f32 {
    36.0 // 0.5"
}

impl Default for Margins {
    fn default() -> Self {
        Margins {
            top: default_margin(),
            right: default_margin(),
            bottom: default_margin(),
            left: default_margin(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Frame {
    #[serde(default)]
    pub id: u64,
    /// bbox in page points; the frame rotates about its own center
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// clockwise degrees
    #[serde(default)]
    pub rotation_deg: f32,
    /// stacking order; higher z paints later (on top)
    #[serde(default)]
    pub z: i32,
    #[serde(flatten)]
    pub kind: FrameKind,
}

impl Frame {
    pub fn new(kind: FrameKind, x: f32, y: f32, w: f32, h: f32) -> Frame {
        Frame {
            id: 0,
            x,
            y,
            w,
            h,
            rotation_deg: 0.0,
            z: 0,
            kind,
        }
    }

    pub fn kind_name(&self) -> &'static str {
        match &self.kind {
            FrameKind::Text { .. } => "text",
            FrameKind::Image { .. } => "image",
            FrameKind::Rect { .. } => "rect",
            FrameKind::Line { .. } => "line",
        }
    }

    /// frame center in page points
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w * 0.5, self.y + self.h * 0.5)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum FrameKind {
    /// flowed text wrapped inside the frame box
    Text {
        text: String,
        /// family name resolved via fontdb, or absolute .ttf/.otf path;
        /// empty / unresolvable falls back to Helvetica
        #[serde(default)]
        font: String,
        /// size in points
        #[serde(default = "default_font_size")]
        size: f32,
        /// [r,g,b,a] 0..1 sRGB
        #[serde(default = "black")]
        color: [f32; 4],
        #[serde(default)]
        align: TextAlign,
        /// line-height multiplier (1.0 = font's natural leading)
        #[serde(default = "default_leading")]
        leading: f32,
    },
    /// an image file placed into the frame
    Image {
        path: PathBuf,
        #[serde(default)]
        fit: ImageFit,
    },
    /// filled and/or stroked rectangle covering the frame box
    Rect {
        #[serde(default)]
        fill: Option<[f32; 4]>,
        #[serde(default)]
        stroke: Option<Stroke>,
    },
    /// straight line inside the frame box, from local (0,0) to (x2,y2) —
    /// (w,h) is a diagonal; (0,h)..(w,0) flips it. Rotation applies as usual.
    Line { x2: f32, y2: f32, stroke: Stroke },
}

fn default_font_size() -> f32 {
    12.0
}
fn default_leading() -> f32 {
    1.2
}
fn black() -> [f32; 4] {
    [0.0, 0.0, 0.0, 1.0]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

impl TextAlign {
    pub fn parse(s: &str) -> TextAlign {
        match s {
            "center" | "c" => TextAlign::Center,
            "right" | "r" => TextAlign::Right,
            _ => TextAlign::Left,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ImageFit {
    /// scale to cover the frame, cropping overflow (default: InDesign
    /// "fill frame proportionally")
    #[default]
    Fill,
    /// scale to fit wholly inside the frame (letterbox)
    Fit,
    /// distort to exactly the frame size
    Stretch,
}

impl ImageFit {
    pub fn parse(s: &str) -> Option<ImageFit> {
        match s {
            "fill" => Some(ImageFit::Fill),
            "fit" => Some(ImageFit::Fit),
            "stretch" => Some(ImageFit::Stretch),
            _ => None,
        }
    }

    /// destination rect (frame-local, pt) an image of `iw`×`ih` pixels
    /// occupies under this fit mode.
    pub fn dest_rect(&self, frame_w: f32, frame_h: f32, iw: u32, ih: u32) -> (f32, f32, f32, f32) {
        let iw = iw.max(1) as f32;
        let ih = ih.max(1) as f32;
        match self {
            ImageFit::Stretch => (0.0, 0.0, frame_w, frame_h),
            ImageFit::Fill | ImageFit::Fit => {
                let s = if *self == ImageFit::Fill {
                    (frame_w / iw).max(frame_h / ih)
                } else {
                    (frame_w / iw).min(frame_h / ih)
                };
                let w = iw * s;
                let h = ih * s;
                ((frame_w - w) * 0.5, (frame_h - h) * 0.5, w, h)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stroke {
    /// [r,g,b,a] 0..1 sRGB
    pub color: [f32; 4],
    /// width in points
    #[serde(default = "one_f")]
    pub width: f32,
}

fn one_f() -> f32 {
    1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_round_trip() {
        let mut d = PagesDoc::new("Test", 612.0, 792.0);
        d.add_master("A-Master");
        d.add_page(Some(0)).unwrap();
        d.add_page(None).unwrap();
        let f = Frame::new(
            FrameKind::Text {
                text: "Hello".into(),
                font: "Helvetica".into(),
                size: 18.0,
                color: [0.1, 0.2, 0.3, 1.0],
                align: TextAlign::Center,
                leading: 1.4,
            },
            36.0,
            36.0,
            200.0,
            100.0,
        );
        d.add_frame(FrameTarget::Page(0), f).unwrap();
        let j = d.to_json();
        let back = PagesDoc::from_json(&j).unwrap();
        assert_eq!(back.pages.len(), 2);
        assert_eq!(back.masters.len(), 1);
        assert_eq!(back.pages[0].master, Some(0));
        assert_eq!(back.pages[0].frames.len(), 1);
        match &back.pages[0].frames[0].kind {
            FrameKind::Text { text, align, .. } => {
                assert_eq!(text, "Hello");
                assert_eq!(*align, TextAlign::Center);
            }
            _ => panic!("wrong kind"),
        }
        // camelCase keys
        assert!(j.contains("\"pageW\""));
        assert!(j.contains("\"rotationDeg\""));
        assert!(j.contains("\"kind\": \"text\""));
    }

    #[test]
    fn frame_geometry() {
        let mut d = PagesDoc::new("G", 100.0, 100.0);
        d.add_page(None).unwrap();
        let id = d
            .add_frame(
                FrameTarget::Page(0),
                Frame::new(
                    FrameKind::Rect {
                        fill: Some([1.0, 0.0, 0.0, 1.0]),
                        stroke: None,
                    },
                    10.0,
                    20.0,
                    30.0,
                    40.0,
                ),
            )
            .unwrap();
        let f = d.frame_mut(id).unwrap();
        f.x += 5.0;
        let (cx, cy) = f.center();
        assert_eq!((cx, cy), (30.0, 40.0));
        assert!(d.remove_frame(id).is_some());
        assert!(d.frame_mut(id).is_none());
        assert!(d.remove_frame(9999).is_none());
    }

    #[test]
    fn resolved_frames_master_under_page_sorted_by_z() {
        let mut d = PagesDoc::new("M", 100.0, 100.0);
        let mi = d.add_master("M");
        d.add_page(Some(mi)).unwrap();
        let rect = |z| Frame {
            z,
            ..Frame::new(
                FrameKind::Rect {
                    fill: Some([0.0; 4]),
                    stroke: None,
                },
                0.0,
                0.0,
                10.0,
                10.0,
            )
        };
        d.add_frame(FrameTarget::Master(mi), rect(5)).unwrap();
        let low = d.add_frame(FrameTarget::Page(0), rect(-1)).unwrap();
        let high = d.add_frame(FrameTarget::Page(0), rect(9)).unwrap();
        let order: Vec<u64> = d.resolved_frames(0).unwrap().iter().map(|f| f.id).collect();
        // master frames always paint below page frames (InDesign semantics);
        // page frames order among themselves by z
        assert_eq!(order, vec![d.masters[mi].frames[0].id, low, high]);
    }

    #[test]
    fn image_fit_rects() {
        // 2:1 image into a 100x100 frame
        let (x, y, w, h) = ImageFit::Fit.dest_rect(100.0, 100.0, 200, 100);
        assert_eq!((x, y, w, h), (0.0, 25.0, 100.0, 50.0));
        let (_, _, w2, h2) = ImageFit::Fill.dest_rect(100.0, 100.0, 200, 100);
        assert_eq!((w2, h2), (200.0, 100.0));
        let (x, y, w, h) = ImageFit::Stretch.dest_rect(100.0, 100.0, 200, 100);
        assert_eq!((x, y, w, h), (0.0, 0.0, 100.0, 100.0));
    }
}
