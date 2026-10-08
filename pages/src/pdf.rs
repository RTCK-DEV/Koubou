//! Hand-rolled PDF 1.4 writer — no external PDF engine.
//!
//! Emits: catalog → page tree → per-page content streams (FlateDecode) +
//! image XObjects + one shared base-14 Helvetica font (WinAnsiEncoding).
//! Page geometry arrives in top-left/y-down points and is flipped into
//! PDF's bottom-left space; frame rotation is a `cm` about the frame center
//! and every frame is clipped to its rect (`W n`).

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use flate2::write::ZlibEncoder;
use flate2::Compression;

use crate::model::{Frame, FrameKind, PagesDoc};
use crate::text::{to_winansi, winansi_covers};

/// render every page of the document to PDF bytes
pub fn render_pdf(doc: &PagesDoc) -> Result<Vec<u8>> {
    if doc.pages.is_empty() {
        anyhow::bail!("document has no pages");
    }
    if !(doc.page_w > 0.0 && doc.page_h > 0.0) {
        anyhow::bail!("bad page size {}×{}", doc.page_w, doc.page_h);
    }

    let mut w = Writer::new();
    let catalog_id = w.reserve();
    let pages_id = w.reserve();
    let font_id = w.push(
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_vec(),
    );

    // threaded-text resolution is document-global (chains may cross pages)
    let flows = crate::flow::resolve_flow(doc)?;

    let mut kids = Vec::with_capacity(doc.pages.len());
    for i in 0..doc.pages.len() {
        kids.push(page(&mut w, doc, i, pages_id, font_id, &flows)?);
    }

    let kids_str: Vec<String> = kids.iter().map(|k| format!("{k} 0 R")).collect();
    w.set(
        pages_id,
        format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>",
            kids_str.join(" "),
            kids.len()
        )
        .into_bytes(),
    );
    w.set(
        catalog_id,
        format!("<< /Type /Catalog /Pages {pages_id} 0 R >>").into_bytes(),
    );
    Ok(w.finish())
}

/// emit one page's objects; returns the /Page object id
fn page(
    w: &mut Writer,
    doc: &PagesDoc,
    idx: usize,
    pages_id: u32,
    font_id: u32,
    flows: &std::collections::HashMap<u64, crate::flow::FrameFlow>,
) -> Result<u32> {
    let page_id = w.reserve();
    let frames = doc.resolved_frames(idx)?;

    let mut content = String::new();
    let mut xobjects: Vec<(String, u32)> = Vec::new(); // (name, obj id)
    let mut extgstates: HashMap<u32, String> = HashMap::new(); // alpha bits -> gs name
    let mut xo_seq = 0usize;

    for f in &frames {
        frame_ops(
            w,
            doc,
            f,
            &mut content,
            &mut xobjects,
            &mut extgstates,
            &mut xo_seq,
            flows,
        )?;
    }

    // content stream (compressed)
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(content.as_bytes())
        .context("deflate contents")?;
    let compressed = enc.finish().context("deflate finish")?;
    let mut cobj = format!(
        "<< /Length {} /Filter /FlateDecode >>\nstream\n",
        compressed.len()
    )
    .into_bytes();
    cobj.extend_from_slice(&compressed);
    cobj.extend_from_slice(b"\nendstream");
    let contents_id = w.push(cobj);

    // deferred extgstate objects now that we know which alphas were used
    let mut gs_res = String::new();
    let mut gs_ids: Vec<(String, u32)> = Vec::new();
    // stable order so output is deterministic
    let mut alphas: Vec<u32> = extgstates.keys().copied().collect();
    alphas.sort_unstable();
    for a in alphas {
        let name = extgstates[&a].clone();
        let alpha = f32::from_bits(a);
        let id = w.push(
            format!(
                "<< /Type /ExtGState /ca {} /CA {} >>",
                num(alpha),
                num(alpha)
            )
            .into_bytes(),
        );
        gs_ids.push((name, id));
    }
    for (name, id) in &gs_ids {
        gs_res.push_str(&format!("/{name} {id} 0 R "));
    }

    let mut res = format!("/ProcSet [/PDF /Text /ImageC] /Font << /F1 {font_id} 0 R >>");
    if !xobjects.is_empty() {
        res.push_str(" /XObject <<");
        for (name, id) in &xobjects {
            res.push_str(&format!(" /{name} {id} 0 R"));
        }
        res.push_str(" >>");
    }
    if !gs_ids.is_empty() {
        res.push_str(&format!(" /ExtGState <<{gs_res}>>"));
    }

    w.set(
        page_id,
        format!(
            "<< /Type /Page /Parent {pages_id} 0 R /MediaBox [0 0 {} {}] /Resources <<{res} >> /Contents {contents_id} 0 R >>",
            num(doc.page_w),
            num(doc.page_h),
        )
        .into_bytes(),
    );
    Ok(page_id)
}

/// content-stream ops for one frame, wrapped in q/Q with clip + rotation
fn frame_ops(
    w: &mut Writer,
    doc: &PagesDoc,
    f: &Frame,
    out: &mut String,
    xobjects: &mut Vec<(String, u32)>,
    extgstates: &mut HashMap<u32, String>,
    xo_seq: &mut usize,
    flows: &std::collections::HashMap<u64, crate::flow::FrameFlow>,
) -> Result<()> {
    out.push_str("q\n");

    // rotation about the frame center (visual clockwise in y-down space
    // becomes this matrix in PDF's y-up space)
    let rot = f.rotation_deg.rem_euclid(360.0);
    if rot.abs() > f32::EPSILON {
        let (cx, cy) = f.center();
        let cyu = doc.page_h - cy;
        let t = rot.to_radians();
        let (c, s) = (t.cos(), t.sin());
        out.push_str(&format!(
            "1 0 0 1 {cx} {cyu} cm {} {} {} {} 0 0 cm 1 0 0 1 {ncx} {ncyu} cm\n",
            num(c),
            num(-s),
            num(s),
            num(c),
            cx = num(cx),
            cyu = num(cyu),
            ncx = num(-cx),
            ncyu = num(-cyu),
        ));
    }

    // clip to the frame rect (in unrotated user space; CTM rotates it)
    let fy = doc.page_h - (f.y + f.h);
    out.push_str(&format!(
        "{} {} {} {} re W n\n",
        num(f.x),
        num(fy),
        num(f.w),
        num(f.h)
    ));

    match &f.kind {
        FrameKind::Rect { fill, stroke } => {
            if let Some(c) = fill {
                alpha_state(out, extgstates, c[3]);
                out.push_str(&format!("{} rg\n", rgb(*c)));
            }
            if let Some(st) = stroke {
                alpha_state(out, extgstates, st.color[3]);
                out.push_str(&format!("{} RG {} w\n", rgb(st.color), num(st.width)));
            }
            let op = match (fill, stroke) {
                (Some(_), Some(_)) => "B",
                (Some(_), None) => "f",
                (None, Some(_)) => "S",
                (None, None) => "n",
            };
            out.push_str(&format!(
                "{} {} {} {} re {op}\n",
                num(f.x),
                num(fy),
                num(f.w),
                num(f.h)
            ));
        }
        FrameKind::Line { x2, y2, stroke } => {
            alpha_state(out, extgstates, stroke.color[3]);
            let (x1, y1) = (f.x, f.y);
            let (px2, py2) = (f.x + x2, f.y + y2);
            out.push_str(&format!(
                "{} RG {} w 1 J\n{} {} m {} {} l S\n",
                rgb(stroke.color),
                num(stroke.width),
                num(x1),
                num(doc.page_h - y1),
                num(px2),
                num(doc.page_h - py2),
            ));
        }
        FrameKind::Image { path, fit } => {
            let name = format!("Im{}", *xo_seq);
            *xo_seq += 1;
            let (iw, ih) = image_dims(path)?;
            let (dx, dy, dw, dh) = fit.dest_rect(f.w, f.h, iw, ih);
            let id = embed_image(w, path)?;
            xobjects.push((name.clone(), id));
            // dest rect in frame-local pt → PDF page coords
            let px = f.x + dx;
            let py = doc.page_h - (f.y + dy + dh);
            out.push_str(&format!(
                "{} 0 0 {} {} {} cm /{name} Do\n",
                num(dw),
                num(dh),
                num(px),
                num(py)
            ));
        }
        FrameKind::Text {
            font, size, color, ..
        } => {
            // the flow map covers every text frame — a missing entry (only
            // possible via a corrupt chain) renders nothing
            if let Some(fl) = flows.get(&f.id) {
                // Fast path: base-14 Helvetica + WinAnsi text ops.
                // Everything else — CJK and other non-WinAnsi runs,
                // non-Helvetica faces, font files — rasterizes through the
                // text engine into an image XObject with an alpha SMask,
                // so the PDF shows the real glyphs instead of '?'.
                let type1_ok =
                    uses_base14(font) && fl.layout.lines.iter().all(|l| winansi_covers(&l.text));
                if type1_ok {
                    alpha_state(out, extgstates, color[3]);
                    out.push_str(&format!("{} rg\nBT /F1 {} Tf\n", rgb(*color), num(*size)));
                    for line in &fl.layout.lines {
                        if line.text.is_empty() {
                            continue;
                        }
                        let tx = f.x + line.x_off;
                        let ty = doc.page_h - (f.y + line.baseline);
                        out.push_str(&format!(
                            "1 0 0 1 {} {} Tm ({}) Tj\n",
                            num(tx),
                            num(ty),
                            pdf_escape(&to_winansi(&line.text))
                        ));
                    }
                    out.push_str("ET\n");
                } else {
                    let img =
                        crate::raster::text_stamp(&fl.layout, *color, f.w, f.h, TEXT_RASTER_SCALE);
                    // skip fully transparent output rather than embedding a
                    // blank image (e.g. only whitespace laid out)
                    if img.pixels().any(|p| p[3] != 0) {
                        let name = format!("Im{}", *xo_seq);
                        *xo_seq += 1;
                        let id = push_rgba_image(w, &img)?;
                        xobjects.push((name.clone(), id));
                        // the stamp covers the whole frame box — text sits
                        // at its laid positions inside it
                        let py = doc.page_h - (f.y + f.h);
                        out.push_str(&format!(
                            "{} 0 0 {} {} {} cm /{name} Do\n",
                            num(f.w),
                            num(f.h),
                            num(f.x),
                            num(py)
                        ));
                    }
                }
            }
        }
    }

    out.push_str("Q\n");
    Ok(())
}

/// px-per-pt for rasterized text — 4× (288 dpi effective): sharp enough for
/// print adjacency while keeping embedded image sizes modest.
const TEXT_RASTER_SCALE: f32 = 4.0;

/// does the font spec map onto the shared Helvetica Type1 resource? An
/// empty spec resolves to Helvetica anyway; explicit non-Helvetica specs
/// rasterize so the requested face (not Helvetica) reaches the PDF.
fn uses_base14(spec: &str) -> bool {
    if spec.is_empty() {
        return true;
    }
    matches!(
        spec.to_ascii_lowercase().as_str(),
        "helvetica" | "arial" | "helvetica neue"
    )
}

/// register an alpha value and emit `/GSn gs`
fn alpha_state(out: &mut String, states: &mut HashMap<u32, String>, alpha: f32) {
    let a = alpha.clamp(0.0, 1.0);
    if a >= 1.0 {
        return;
    }
    let bits = a.to_bits();
    let name = match states.get(&bits) {
        Some(n) => n.clone(),
        None => {
            let n = format!("GS{}", states.len());
            states.insert(bits, n.clone());
            n
        }
    };
    out.push_str(&format!("/{name} gs\n"));
}

/// [r,g,b,a] → "r g b" pdf color operands
fn rgb(c: [f32; 4]) -> String {
    format!(
        "{} {} {}",
        num(c[0].clamp(0.0, 1.0)),
        num(c[1].clamp(0.0, 1.0)),
        num(c[2].clamp(0.0, 1.0))
    )
}

/// PDF literal-string escaping over raw WinAnsi bytes
fn pdf_escape(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() + 8);
    for &b in bytes {
        match b {
            b'(' => s.push_str("\\("),
            b')' => s.push_str("\\)"),
            b'\\' => s.push_str("\\\\"),
            0x00..=0x1F | 0x7F..=0xFF => s.push_str(&format!("\\{:03o}", b)),
            _ => s.push(b as char),
        }
    }
    s
}

/// compact float formatting: 4 decimal places, no trailing zeros
fn num(v: f32) -> String {
    if !v.is_finite() {
        return "0".to_string();
    }
    let s = format!("{:.4}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" || s.is_empty() {
        "0".to_string()
    } else {
        s.to_string()
    }
}

/// (width, height, components) of a JPEG, read from its SOF marker.
/// components: 1 = gray, 3 = YCbCr RGB, 4 = CMYK.
fn jpeg_dims(data: &[u8]) -> Result<(u32, u32, u8)> {
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        anyhow::bail!("not a JPEG (bad SOI)");
    }
    let mut i = 2usize;
    while i + 4 <= data.len() {
        if data[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = data[i + 1];
        // standalone markers without a length field
        if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            i += 2;
            continue;
        }
        let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        if len < 2 || i + 2 + len > data.len() {
            anyhow::bail!("truncated JPEG segment");
        }
        // SOF markers (excluding DHT/DAC/JPGn/etc.)
        if matches!(
            marker,
            0xC0 | 0xC1
                | 0xC2
                | 0xC3
                | 0xC5
                | 0xC6
                | 0xC7
                | 0xC9
                | 0xCA
                | 0xCB
                | 0xCD
                | 0xCE
                | 0xCF
        ) {
            if len < 8 {
                anyhow::bail!("short SOF");
            }
            let h = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
            let w = u16::from_be_bytes([data[i + 7], data[i + 8]]) as u32;
            let comps = data[i + 9];
            return Ok((w, h, comps));
        }
        i += 2 + len;
    }
    anyhow::bail!("no SOF marker in JPEG")
}

/// image pixel dimensions without a full decode when the source is JPEG
fn image_dims(path: &Path) -> Result<(u32, u32)> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xD8 {
        let (w, h, _) = jpeg_dims(&bytes)?;
        return Ok((w, h));
    }
    image::image_dimensions(path).with_context(|| format!("image dims {}", path.display()))
}

/// embed an image file as an XObject; returns its object id.
/// JPEG sources are passed through verbatim as DCTDecode; anything else is
/// decoded to RGB8 (+ optional SMask alpha) and FlateDecode-compressed.
fn embed_image(w: &mut Writer, path: &Path) -> Result<u32> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xD8 {
        let (wid, hei, comps) = jpeg_dims(&bytes)?;
        let cs = match comps {
            1 => "/DeviceGray",
            4 => "/DeviceCMYK",
            _ => "/DeviceRGB",
        };
        let decode = if comps == 4 {
            " /Decode [1 0 1 0 1 0 1 0]"
        } else {
            ""
        };
        let mut obj = format!(
            "<< /Type /XObject /Subtype /Image /Width {wid} /Height {hei} /ColorSpace {cs} /BitsPerComponent 8 /Filter /DCTDecode{decode} /Length {} >>\nstream\n",
            bytes.len()
        )
        .into_bytes();
        obj.extend_from_slice(&bytes);
        obj.extend_from_slice(b"\nendstream");
        return Ok(w.push(obj));
    }

    let img = image::open(path)
        .with_context(|| format!("decode image {}", path.display()))?
        .to_rgba8();
    push_rgba_image(w, &img)
}

/// embed raw RGBA8 pixels as an image XObject; an /SMask (DeviceGray,
/// FlateDecode) is added whenever any alpha byte is < 255. Returns the
/// object id. Shared by file images and rasterized text.
fn push_rgba_image(w: &mut Writer, img: &image::RgbaImage) -> Result<u32> {
    let (wid, hei) = img.dimensions();
    let raw = img.as_raw();
    let (pixels, _) = raw.as_chunks::<4>();
    let has_alpha = pixels.iter().any(|p| p[3] != 255);

    let mut rgb_data = Vec::with_capacity((wid * hei * 3) as usize);
    for p in pixels {
        rgb_data.extend_from_slice(&p[..3]);
    }
    let rgb_enc = deflate(&rgb_data)?;

    let smask_ref = if has_alpha {
        let alpha: Vec<u8> = pixels.iter().map(|p| p[3]).collect();
        let a_enc = deflate(&alpha)?;
        let mut sobj = format!(
            "<< /Type /XObject /Subtype /Image /Width {wid} /Height {hei} /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode /Length {} >>\nstream\n",
            a_enc.len()
        )
        .into_bytes();
        sobj.extend_from_slice(&a_enc);
        sobj.extend_from_slice(b"\nendstream");
        format!(" /SMask {} 0 R", w.push(sobj))
    } else {
        String::new()
    };

    let mut obj = format!(
        "<< /Type /XObject /Subtype /Image /Width {wid} /Height {hei} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode{smask_ref} /Length {} >>\nstream\n",
        rgb_enc.len()
    )
    .into_bytes();
    obj.extend_from_slice(&rgb_enc);
    obj.extend_from_slice(b"\nendstream");
    Ok(w.push(obj))
}

fn deflate(data: &[u8]) -> Result<Vec<u8>> {
    let mut e = ZlibEncoder::new(Vec::new(), Compression::default());
    e.write_all(data).context("deflate image")?;
    e.finish().context("deflate finish")
}

/// byte-level object assembler: reserves ids, tracks offsets, emits xref.
struct Writer {
    /// (id-1) → object payload bytes (with `stream` blocks inline)
    objs: Vec<Vec<u8>>,
}

impl Writer {
    fn new() -> Writer {
        Writer { objs: Vec::new() }
    }

    fn reserve(&mut self) -> u32 {
        self.objs.push(Vec::new());
        self.objs.len() as u32
    }

    fn push(&mut self, payload: Vec<u8>) -> u32 {
        self.objs.push(payload);
        self.objs.len() as u32
    }

    fn set(&mut self, id: u32, payload: Vec<u8>) {
        if let Some(slot) = self.objs.get_mut((id - 1) as usize) {
            *slot = payload;
        }
    }

    fn finish(self) -> Vec<u8> {
        let mut out: Vec<u8> = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
        let mut offsets = Vec::with_capacity(self.objs.len());
        for (i, payload) in self.objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            out.extend_from_slice(payload);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref_pos = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", self.objs.len() + 1).as_bytes(),
        );
        for off in offsets {
            out.extend_from_slice(format!("{:010} 00000 n \n", off).as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
                self.objs.len() + 1,
                xref_pos
            )
            .as_bytes(),
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Frame, FrameTarget, TextAlign};

    fn doc_with_frame(kind: FrameKind) -> PagesDoc {
        let mut d = PagesDoc::new("T", 200.0, 200.0);
        d.add_page(None).unwrap();
        d.add_frame(
            FrameTarget::Page(0),
            Frame::new(kind, 10.0, 10.0, 100.0, 50.0),
        )
        .unwrap();
        d
    }

    #[test]
    fn jpeg_header_parse() {
        // minimal JPEG: SOI, APP0, SOF0 (8x4, 3 comps), EOI
        let mut j = vec![0xFF, 0xD8];
        j.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x10]);
        j.extend_from_slice(&[0u8; 14]);
        j.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x04, 0x00, 0x08, 0x03]);
        j.extend_from_slice(&[0x01, 0x22, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11, 0x01]);
        j.extend_from_slice(&[0xFF, 0xD9]);
        assert_eq!(jpeg_dims(&j).unwrap(), (8, 4, 3));
        assert!(jpeg_dims(b"notajpeg").is_err());
    }

    /// decompress the first *content* stream (a page's op stream — not an
    /// image stream, which has /Subtype /Image in its dict)
    fn first_content_stream(pdf: &[u8]) -> String {
        let mut pos = 0usize;
        while let Some(p) = pdf[pos..].windows(7).position(|w| w == b"stream\n") {
            let start = pos + p + 7;
            let hdr_end = pdf[..start]
                .windows(2)
                .rposition(|w| w == b"<<")
                .unwrap_or(0);
            let header = String::from_utf8_lossy(&pdf[hdr_end..start]);
            if header.contains("/Subtype /Image") {
                pos = start;
                continue;
            }
            let end = pdf[start..]
                .windows(9)
                .position(|w| w == b"endstream")
                .map(|p| start + p)
                .unwrap();
            let mut dec = flate2::read::ZlibDecoder::new(&pdf[start..end]);
            let mut s = String::new();
            std::io::Read::read_to_string(&mut dec, &mut s).unwrap();
            return s;
        }
        panic!("no content stream found");
    }

    /// inflate the stream of the first object containing `marker`, honoring
    /// its /Length (compressed bytes may contain "endstream" by chance)
    fn inflate_stream_after(pdf: &[u8], marker: &[u8]) -> Vec<u8> {
        let mpos = pdf
            .windows(marker.len())
            .position(|w| w == marker)
            .unwrap_or_else(|| panic!("marker {:?} not found", String::from_utf8_lossy(marker)));
        let after = &pdf[mpos..];
        let sstart = after
            .windows(7)
            .position(|w| w == b"stream\n")
            .map(|p| p + 7)
            .unwrap();
        // /Length sits in the dict just above the stream
        let hdr = String::from_utf8_lossy(&after[..sstart.min(after.len())]);
        let lpos = hdr.rfind("/Length ").expect("no /Length");
        let digits: String = hdr[lpos + 8..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        let len: usize = digits.parse().expect("bad /Length");
        let data = &after[sstart..sstart + len];
        let mut dec = flate2::read::ZlibDecoder::new(data);
        let mut out = Vec::new();
        std::io::Read::read_to_end(&mut dec, &mut out).unwrap();
        out
    }

    #[test]
    fn pdf_structure() {
        let d = doc_with_frame(FrameKind::Rect {
            fill: Some([1.0, 0.0, 0.0, 1.0]),
            stroke: None,
        });
        let pdf = render_pdf(&d).unwrap();
        assert!(pdf.starts_with(b"%PDF-"));
        let s = String::from_utf8_lossy(&pdf);
        assert!(s.contains("/Type /Page"));
        assert!(s.contains("/Font"));
        assert!(s.contains("xref"));
        assert!(s.contains("%%EOF"));
        // content stream is compressed — inflate to check the draw ops
        let c = first_content_stream(&pdf);
        assert!(c.contains("re f"), "missing rect fill op:\n{c}");
        assert!(c.contains("W n"), "missing frame clip:\n{c}");
    }

    #[test]
    fn page_count_in_pdf() {
        let mut d = PagesDoc::new("N", 100.0, 100.0);
        for _ in 0..3 {
            d.add_page(None).unwrap();
        }
        let pdf = render_pdf(&d).unwrap();
        let s = String::from_utf8_lossy(&pdf);
        assert!(s.contains("/Count 3"));
    }

    #[test]
    fn text_ops_emitted() {
        let d = doc_with_frame(FrameKind::Text {
            text: "hello world".into(),
            font: String::new(),
            size: 12.0,
            color: [0.0, 0.0, 0.0, 1.0],
            align: TextAlign::Left,
            leading: 1.2,
            style: String::new(),
        });
        let pdf = render_pdf(&d).unwrap();
        let s = String::from_utf8_lossy(&pdf);
        assert!(s.contains("/BaseFont /Helvetica"));
        let c = first_content_stream(&pdf);
        assert!(c.contains("(hello world) Tj"), "no text op:\n{c}");
        assert!(c.contains("/F1 12 Tf"), "no font select:\n{c}");
    }

    #[test]
    fn jpeg_passthrough_dct() {
        // build a real JPEG via the image crate, then check the PDF embeds
        // its bytes verbatim under DCTDecode
        let dir = std::env::temp_dir().join(format!("kpages_jpeg_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let jp = dir.join("img.jpg");
        let img = image::RgbaImage::from_fn(16, 8, |x, y| {
            image::Rgba([(x * 16) as u8, (y * 30) as u8, 200, 255])
        });
        image::DynamicImage::ImageRgba8(img)
            .save_with_format(&jp, image::ImageFormat::Jpeg)
            .unwrap();
        let jpeg_bytes = std::fs::read(&jp).unwrap();

        let mut d = PagesDoc::new("J", 200.0, 200.0);
        d.add_page(None).unwrap();
        d.add_frame(
            FrameTarget::Page(0),
            Frame::new(
                FrameKind::Image {
                    path: jp.clone(),
                    fit: crate::model::ImageFit::Stretch,
                },
                0.0,
                0.0,
                100.0,
                50.0,
            ),
        )
        .unwrap();
        let pdf = render_pdf(&d).unwrap();
        let s = String::from_utf8_lossy(&pdf);
        assert!(s.contains("/DCTDecode"), "jpeg not passed through");
        assert!(s.contains("/Width 16 /Height 8"));
        // the raw jpeg bytes must appear verbatim
        assert!(
            pdf.windows(jpeg_bytes.len())
                .any(|w| w == jpeg_bytes.as_slice()),
            "jpeg payload not embedded verbatim"
        );
    }

    #[test]
    fn cjk_text_embeds_smask_image_not_question_marks() {
        // "建築確認申請" is not WinAnsi-representable: the run must rasterize
        // into an image XObject + alpha SMask instead of '(???) Tj'
        let d = doc_with_frame(FrameKind::Text {
            text: "建築確認申請".into(),
            font: String::new(),
            size: 24.0,
            color: [0.0, 0.0, 0.0, 1.0],
            align: TextAlign::Left,
            leading: 1.2,
            style: String::new(),
        });
        let pdf = render_pdf(&d).unwrap();
        let s = String::from_utf8_lossy(&pdf);
        assert!(s.contains("/SMask"), "rasterized text needs an alpha SMask");
        assert!(
            s.contains("/Subtype /Image"),
            "rasterized text needs an image XObject"
        );
        // content stream draws an image, never a WinAnsi text op
        let c = first_content_stream(&pdf);
        assert!(c.contains(" Do"), "expected an image draw op:\n{c}");
        assert!(
            !c.contains("Tj"),
            "CJK must not take the WinAnsi path:\n{c}"
        );
        // the gray alpha stream must carry real coverage
        let smask = inflate_stream_after(&pdf, b"/DeviceGray");
        assert!(
            smask.iter().filter(|&&b| b != 0).count() > 100,
            "smask coverage too small — glyphs missing"
        );
        // non-Helvetica specs also rasterize (even for latin) so the
        // requested face reaches the PDF, not Helvetica
        let d2 = doc_with_frame(FrameKind::Text {
            text: "styled latin".into(),
            font: "Futura".into(),
            size: 12.0,
            color: [0.0, 0.0, 0.0, 1.0],
            align: TextAlign::Left,
            leading: 1.2,
            style: String::new(),
        });
        let pdf2 = render_pdf(&d2).unwrap();
        let c2 = first_content_stream(&pdf2);
        assert!(c2.contains(" Do"), "non-Helvetica should rasterize:\n{c2}");
        assert!(!c2.contains("(styled latin) Tj"));
    }

    #[test]
    fn empty_doc_rejected() {
        let d = PagesDoc::new("E", 100.0, 100.0);
        assert!(render_pdf(&d).is_err());
    }

    #[test]
    fn rotation_emits_cm() {
        let mut d = PagesDoc::new("R", 100.0, 100.0);
        d.add_page(None).unwrap();
        let mut f = Frame::new(
            FrameKind::Rect {
                fill: Some([0.0, 0.0, 1.0, 1.0]),
                stroke: None,
            },
            10.0,
            10.0,
            20.0,
            20.0,
        );
        f.rotation_deg = 45.0;
        d.add_frame(FrameTarget::Page(0), f).unwrap();
        let pdf = render_pdf(&d).unwrap();
        let s = first_content_stream(&pdf);
        assert!(s.contains("cm"), "no transform emitted:\n{s}");
        assert!(s.contains("0.7071"), "expected 45° cos/sin:\n{s}");
    }
}
