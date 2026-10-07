import SwiftUI
import AppKit

// MARK: - Document store

/// A layer row parsed out of doc.json for the panel/inspector.
struct DocLayer: Identifiable, Equatable {
    let id: UInt64
    var name: String
    var kind: String        // develop|raster|adjustment|fill|shape|text|group
    var visible: Bool
    var opacity: Double
    var blend: String
    var x: Double
    var y: Double
    var scale: Double
    var hasMask: Bool
    var recipe: Recipe?
    var text: [String: Any]?
    var fill: [String: Any]?
    var shapes: [[String: Any]]?

    static func == (a: DocLayer, b: DocLayer) -> Bool {
        a.id == b.id && a.name == b.name && a.kind == b.kind
            && a.visible == b.visible && a.opacity == b.opacity
            && a.blend == b.blend && a.hasMask == b.hasMask
    }
}

/// Owns a DocSession and the published state of the open document.
final class DocStore: ObservableObject {
    private var session: DocSession?
    @Published var layers: [DocLayer] = []
    @Published var docName = ""
    @Published var docW = 0
    @Published var docH = 0
    @Published var composite: CGImage?
    @Published var selected: UInt64?
    @Published var busy = false
    @Published var dirty = false
    @Published var error: String?
    @Published var docPath: String?
    // mask painting
    @Published var maskArmed = false
    @Published var brushRadius: Double = 48
    @Published var brushErase = true   // true = hide, false = reveal
    let previewMaxPx: UInt32 = 1600

    private var pendingImageReload = false

    var selLayer: DocLayer? { layers.first { $0.id == selected } }

    private func ensure() -> DocSession? {
        if session == nil { session = DocSession() }
        return session
    }

    // ---- lifecycle ----

    func fromPhoto(_ path: String) {
        dispatch(["id": "doc.fromPhoto", "path": path], then: .reload)
    }

    func open(_ url: URL) {
        let ext = url.pathExtension.lowercased()
        if ext == "psd" {
            dispatch(["id": "doc.importPsd", "path": url.path], then: .reload)
            docPath = nil
        } else {
            dispatch(["id": "doc.open", "path": url.path], then: .reload)
            docPath = url.path
        }
    }

    func newDoc(w: Int = 1920, h: Int = 1080) {
        dispatch(["id": "doc.new", "name": "Untitled", "w": w, "h": h], then: .reload)
        docPath = nil
    }

    func save() {
        if let p = docPath {
            dispatch(["id": "doc.save", "path": p], then: .none)
            dirty = false
        } else { saveAs() }
    }

    func saveAs() {
        let p = NSSavePanel()
        p.allowedContentTypes = [.init(filenameExtension: "koubou") ?? .json]
        p.nameFieldStringValue = (docName.isEmpty ? "Untitled" : docName) + ".koubou"
        guard p.runModal() == .OK, let url = p.url else { return }
        docPath = url.path
        dispatch(["id": "doc.save", "path": url.path], then: .none)
        dirty = false
    }

    func exportPNG() {
        let p = NSSavePanel()
        p.allowedContentTypes = [.png]
        p.nameFieldStringValue = (docName.isEmpty ? "Untitled" : docName) + ".png"
        guard p.runModal() == .OK, let url = p.url else { return }
        dispatch(["id": "doc.render", "out": url.path], then: .none)
    }

    // ---- dispatch plumbing ----

    enum After { case none, reload, reloadImage }

    /// Send a command on the session queue, then optionally refresh state.
    func dispatch(_ cmd: [String: Any], then: After = .reload) {
        guard let s = ensure() else { error = "session failed to start"; return }
        busy = true
        Task.detached { [weak self] in
            let r = await s.work { $0.dispatch(cmd) }
            await MainActor.run {
                guard let self else { return }
                self.busy = false
                if r["ok"] as? Bool == true {
                    self.error = nil
                    let id = cmd["id"] as? String ?? ""
                    if !["doc.render", "doc.json", "doc.save", "doc.maskPaint"].contains(id) {
                        self.dirty = true
                    }
                    switch then {
                    case .none: break
                    case .reload: self.reloadState()
                    case .reloadImage: self.reloadImage()
                    }
                } else {
                    self.error = r["error"] as? String ?? "command failed"
                }
            }
        }
    }

    /// Batch: doc.json + preview render in one queue hop.
    func reloadState() {
        guard let s = session else { return }
        Task.detached { [weak self] in
            let (doc, img) = await s.work { sess -> ([String: Any], CGImage?) in
                let dj = sess.dispatch(["id": "doc.json"])["result"] as? [String: Any] ?? [:]
                let img = Self.decodePNG(sess.dispatch(
                    ["id": "doc.render", "maxPx": self?.previewMaxPx ?? 1600]))
                return (dj, img)
            }
            await MainActor.run { self?.applyState(doc, img) }
        }
    }

    func reloadImage() {
        guard let s = session else { return }
        Task.detached { [weak self] in
            let img = await s.work { sess in
                Self.decodePNG(sess.dispatch(
                    ["id": "doc.render", "maxPx": self?.previewMaxPx ?? 1600]))
            }
            await MainActor.run { self?.composite = img }
        }
    }

    /// Coalesced repaint — used by continuous edits (mask brush, sliders):
    /// at most one pending reload, so a drag doesn't queue a render per event.
    func reloadImageSoon() {
        guard !pendingImageReload else { return }
        pendingImageReload = true
        Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: 180_000_000)
            self?.pendingImageReload = false
            self?.reloadImage()
        }
    }

    private func applyState(_ doc: [String: Any], _ img: CGImage?) {
        docName = doc["name"] as? String ?? ""
        docW = (doc["width"] as? NSNumber)?.intValue ?? 0
        docH = (doc["height"] as? NSNumber)?.intValue ?? 0
        if let img { composite = img }
        let arr = doc["layers"] as? [[String: Any]] ?? []
        layers = arr.compactMap { Self.parseLayer($0) }
        if selected == nil { selected = layers.last?.id }
        if let sel = selected, !layers.contains(where: { $0.id == sel }) {
            selected = layers.last?.id
        }
    }

    static func decodePNG(_ r: [String: Any]) -> CGImage? {
        guard let res = r["result"] as? [String: Any],
              let b64 = res["pngB64"] as? String,
              let data = Data(base64Encoded: b64),
              let ns = NSImage(data: data)
        else { return nil }
        var rect = CGRect(origin: .zero, size: ns.size)
        return ns.cgImage(forProposedRect: &rect, context: nil, hints: nil)
    }

    static func parseLayer(_ d: [String: Any]) -> DocLayer? {
        guard let id = (d["id"] as? NSNumber)?.uint64Value else { return nil }
        var l = DocLayer(
            id: id,
            name: d["name"] as? String ?? "Layer",
            kind: d["type"] as? String ?? "raster",
            visible: d["visible"] as? Bool ?? true,
            opacity: (d["opacity"] as? NSNumber)?.doubleValue ?? 1,
            blend: d["blend"] as? String ?? "normal",
            x: (d["x"] as? NSNumber)?.doubleValue ?? 0,
            y: (d["y"] as? NSNumber)?.doubleValue ?? 0,
            scale: (d["scale"] as? NSNumber)?.doubleValue ?? 1,
            hasMask: d["mask"] != nil && !(d["mask"] is NSNull),
            recipe: nil, text: nil, fill: nil, shapes: nil)
        if let rj = d["recipe"],
           let data = try? JSONSerialization.data(withJSONObject: rj) {
            l.recipe = try? JSONDecoder().decode(Recipe.self, from: data)
        }
        l.text = d["text"] as? [String: Any]
        l.fill = d["fill"] as? [String: Any]
        l.shapes = d["shapes"] as? [[String: Any]]
        return l
    }

    // ---- layer ops ----

    func setLayer(_ id: UInt64, _ params: [String: Any], then: After = .reload) {
        var cmd: [String: Any] = ["id": "doc.setLayer", "layer": id]
        cmd.merge(params) { _, new in new }
        dispatch(cmd, then: then)
    }

    /// Throttled setLayer for slider-driven edits: coalesces a drag into one
    /// dispatch + one composite repaint.
    private var throttleSeq = 0
    func setLayerThrottled(_ id: UInt64, _ params: [String: Any]) {
        throttleSeq += 1
        let n = throttleSeq
        Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: 130_000_000)
            guard let self, n == self.throttleSeq else { return }
            self.setLayer(id, params, then: .reloadImage)
        }
    }

    func addLayer(kind: String) {
        var cmd: [String: Any] = ["id": "doc.addLayer", "kind": kind]
        switch kind {
        case "fill":
            cmd["name"] = "Fill"; cmd["color"] = [0.5, 0.5, 0.5, 1]
        case "gradient":
            cmd["name"] = "Gradient"
            cmd["line"] = [0, 0, docW, docH]
            cmd["stops"] = [[0, 0.12, 0.12, 0.16, 1], [1, 0.28, 0.20, 0.12, 1]]
        case "adjustment":
            cmd["name"] = "Adjustment"; cmd["recipe"] = [String: Any]()
        case "text":
            cmd["name"] = "Text"
            cmd["text"] = ["text": "Text", "size": Double(docH) / 8.0,
                           "color": [1, 1, 1, 1]]
            cmd["x"] = Int(Double(docW) * 0.1); cmd["y"] = Int(Double(docH) * 0.4)
        case "shape":
            cmd["name"] = "Shape"
            let w = Double(docW) / 4, h = Double(docH) / 4
            let cx = Double(docW) / 2, cy = Double(docH) / 2
            cmd["shapes"] = [[
                "d": "M \(cx - w/2) \(cy - h/2) L \(cx + w/2) \(cy - h/2) L \(cx + w/2) \(cy + h/2) L \(cx - w/2) \(cy + h/2) Z",
                "fill": [0.96, 0.66, 0.24, 1]]]
        default:
            cmd["name"] = "Group"
        }
        dispatch(cmd)
    }

    func addPhotoLayer() {
        let p = NSOpenPanel()
        p.canChooseFiles = true; p.canChooseDirectories = false
        guard p.runModal() == .OK, let url = p.url else { return }
        let ext = url.pathExtension.lowercased()
        let kind = ["jpg", "jpeg", "png", "tiff", "tif", "webp", "bmp", "gif"].contains(ext)
            ? "rasterFile" : "develop"
        dispatch(["id": "doc.addLayer", "kind": kind, "path": url.path,
                  "name": url.deletingPathExtension().lastPathComponent])
    }

    func removeLayer(_ id: UInt64) { dispatch(["id": "doc.removeLayer", "layer": id]) }

    /// layers are bottom→top; dir +1 = up (toward top of stack)
    func moveLayer(_ id: UInt64, _ dir: Int) {
        guard let i = layers.firstIndex(where: { $0.id == id }) else { return }
        let to = i + dir
        guard to >= 0, to < layers.count else { return }
        dispatch(["id": "doc.reorder", "layer": id, "to": to])
    }

    func setRecipe(_ id: UInt64, _ recipe: Recipe) {
        guard let data = try? JSONEncoder().encode(recipe),
              let obj = try? JSONSerialization.jsonObject(with: data)
        else { return }
        dispatch(["id": "doc.setLayer", "layer": id, "recipe": obj])
    }

    /// Continuous mask stroke: dispatch the dab immediately (cheap buffer
    /// edit) and coalesce the composite repaint.
    func paintMask(cx: Double, cy: Double) {
        guard let id = selected else { return }
        dispatch(["id": "doc.maskPaint", "layer": id,
                  "cx": cx, "cy": cy, "r": brushRadius,
                  "value": brushErase ? 0.0 : 1.0, "softness": 0.6],
                 then: .none)
        reloadImageSoon()
    }
}

// MARK: - Blend modes

let kBlendModes: [(key: String, label: String)] = [
    ("normal", "Normal"), ("dissolve", "Dissolve"),
    ("darken", "Darken"), ("multiply", "Multiply"),
    ("colorBurn", "Color Burn"), ("linearBurn", "Linear Burn"), ("darkerColor", "Darker Color"),
    ("lighten", "Lighten"), ("screen", "Screen"),
    ("colorDodge", "Color Dodge"), ("linearDodge", "Linear Dodge (Add)"), ("lighterColor", "Lighter Color"),
    ("overlay", "Overlay"), ("softLight", "Soft Light"), ("hardLight", "Hard Light"),
    ("vividLight", "Vivid Light"), ("linearLight", "Linear Light"), ("pinLight", "Pin Light"),
    ("hardMix", "Hard Mix"), ("difference", "Difference"), ("exclusion", "Exclusion"),
    ("subtract", "Subtract"), ("divide", "Divide"),
    ("hue", "Hue"), ("saturation", "Saturation"), ("color", "Color"), ("luminosity", "Luminosity"),
]

func blendLabel(_ key: String) -> String {
    kBlendModes.first { $0.key.lowercased() == key.lowercased() }?.label ?? key
}

let kKindIcons: [String: String] = [
    "develop": "camera.aperture", "raster": "photo", "adjustment": "slider.horizontal.3",
    "fill": "paintbucket", "shape": "square.on.square", "text": "textformat",
    "group": "folder",
]

// MARK: - Editor view

struct DocEditorView: View {
    @ObservedObject var doc: DocStore
    var onClose: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            toolbar
            HSplitView {
                canvas
                    .frame(minWidth: 400)
                rightPanel
                    .frame(width: 330)
            }
        }
        .background(Kou.bg0)
        .preferredColorScheme(.dark)
    }

    private var toolbar: some View {
        HStack(spacing: 10) {
            Button(action: onClose) {
                Image(systemName: "chevron.left").font(.system(size: 11, weight: .semibold))
            }
            .buttonStyle(KouSecondaryButton())
            .help("Back to library")
            Text(doc.docName.isEmpty ? "Untitled" : doc.docName)
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(Kou.text1)
            if doc.dirty {
                Circle().fill(Kou.accent).frame(width: 6, height: 6)
                    .help("Unsaved changes")
            }
            Text("\(doc.docW) × \(doc.docH)")
                .font(.system(size: 10).monospacedDigit())
                .foregroundStyle(Kou.text3)
            Spacer()
            if doc.busy { ProgressView().controlSize(.small).tint(Kou.accent) }
            if let e = doc.error {
                Text(e).font(.system(size: 10)).foregroundStyle(.red.opacity(0.9)).lineLimit(1)
            }
            Button("Save…") { doc.save() }
                .buttonStyle(KouSecondaryButton())
            Button("Export PNG…") { doc.exportPNG() }
                .buttonStyle(KouPrimaryButton())
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(Kou.bg1)
        .overlay(alignment: .bottom) { Kou.hairline.frame(height: 1) }
        .background(
            // cmd+S shortcut
            Button("") { doc.save() }
                .keyboardShortcut("s", modifiers: .command)
                .opacity(0).frame(width: 0, height: 0)
        )
    }

    // ---- canvas ----

    @State private var hoverPoint: CGPoint = .zero

    private var canvas: some View {
        GeometryReader { geo in
            let rect = imageRect(in: geo.size)
            ZStack {
                Checkerboard()
                    .clipShape(RoundedRectangle(cornerRadius: 4))
                if let img = doc.composite {
                    Image(decorative: img, scale: 1)
                        .resizable()
                        .frame(width: rect.width, height: rect.height)
                        .position(x: rect.midX, y: rect.midY)
                } else {
                    ProgressView().tint(Kou.accent)
                        .position(x: geo.size.width / 2, y: geo.size.height / 2)
                }
                if doc.maskArmed {
                    Circle()
                        .stroke(Kou.accent, lineWidth: 1.5)
                        .frame(width: brushViewSize(in: geo.size),
                               height: brushViewSize(in: geo.size))
                        .position(hoverPoint)
                        .allowsHitTesting(false)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(Kou.bg0)
            .contentShape(Rectangle())
            .gesture(
                DragGesture(minimumDistance: 0)
                    .onChanged { g in
                        guard doc.maskArmed else { return }
                        let pt = docPoint(g.location, in: geo.size)
                        doc.paintMask(cx: Double(pt.x), cy: Double(pt.y))
                    }
            )
            .onContinuousHover { phase in
                switch phase {
                case .active(let p): hoverPoint = p
                case .ended: break
                }
            }
        }
        .clipped()
    }

    private func imageRect(in size: CGSize) -> CGRect {
        guard let img = doc.composite, doc.docW > 0, doc.docH > 0 else {
            return CGRect(origin: .zero, size: size)
        }
        let iw = CGFloat(img.width), ih = CGFloat(img.height)
        let sc = min(size.width / iw, size.height / ih) * 0.96
        let w = iw * sc, h = ih * sc
        return CGRect(x: (size.width - w) / 2, y: (size.height - h) / 2,
                      width: w, height: h)
    }

    /// view point → document pixel coords
    private func docPoint(_ p: CGPoint, in size: CGSize) -> CGPoint {
        let rect = imageRect(in: size)
        guard let img = doc.composite, rect.width > 0, doc.docW > 0 else { return .zero }
        let sx = Double(doc.docW) / Double(img.width)
        return CGPoint(x: Double(p.x - rect.minX) * sx,
                       y: Double(p.y - rect.minY) * sx)
    }

    private func brushViewSize(in size: CGSize) -> CGFloat {
        let rect = imageRect(in: size)
        guard doc.docW > 0 else { return 20 }
        return max(6, CGFloat(doc.brushRadius * 2) * rect.width / CGFloat(doc.docW))
    }

    // ---- right panel ----

    private var rightPanel: some View {
        VStack(spacing: 0) {
            layersPanel
                .frame(minHeight: 180, idealHeight: 250, maxHeight: 320)
            Divider().overlay(Kou.hairline)
            inspector
        }
        .background(Kou.bg1)
    }

    private var layersPanel: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                Text("LAYERS")
                    .font(.system(size: 9.5, weight: .semibold)).tracking(1.2)
                    .foregroundStyle(Kou.text2)
                Spacer()
                Menu {
                    Button("Photo / RAW…") { doc.addPhotoLayer() }
                    Button("Adjustment") { doc.addLayer(kind: "adjustment") }
                    Button("Text") { doc.addLayer(kind: "text") }
                    Button("Shape") { doc.addLayer(kind: "shape") }
                    Button("Gradient Fill") { doc.addLayer(kind: "gradient") }
                    Button("Solid Fill") { doc.addLayer(kind: "fill") }
                } label: {
                    Image(systemName: "plus").font(.system(size: 10, weight: .bold))
                        .foregroundStyle(Kou.accent)
                }
                .menuStyle(.borderlessButton)
                .fixedSize()
                Button { if let s = doc.selected { doc.moveLayer(s, -1) } } label: {
                    Image(systemName: "chevron.down").font(.system(size: 9, weight: .bold))
                }
                .buttonStyle(.plain).foregroundStyle(Kou.text2)
                .help("Move down (toward backdrop)")
                Button { if let s = doc.selected { doc.moveLayer(s, 1) } } label: {
                    Image(systemName: "chevron.up").font(.system(size: 9, weight: .bold))
                }
                .buttonStyle(.plain).foregroundStyle(Kou.text2)
                .help("Move up")
                Button { if let s = doc.selected { doc.removeLayer(s) } } label: {
                    Image(systemName: "trash").font(.system(size: 10))
                }
                .buttonStyle(.plain).foregroundStyle(Kou.text2)
            }
            .padding(.horizontal, 10).padding(.vertical, 7)
            .overlay(alignment: .bottom) { Kou.hairline.frame(height: 1) }

            ScrollView {
                VStack(spacing: 2) {
                    ForEach(doc.layers.reversed()) { l in
                        LayerRow(doc: doc, layer: l, selected: doc.selected == l.id)
                            .onTapGesture { doc.selected = l.id }
                    }
                    if doc.layers.isEmpty {
                        Text("No layers — add one with +")
                            .font(.system(size: 10)).foregroundStyle(Kou.text3)
                            .padding(.top, 16)
                    }
                }
                .padding(6)
            }
        }
    }

    private var inspector: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 10) {
                if let l = doc.selLayer {
                    LayerInspector(doc: doc, layer: l)
                        .id(l.id)   // fresh @State per layer — no stale recipe/name
                } else {
                    Text("Select a layer")
                        .font(.system(size: 11)).foregroundStyle(Kou.text3)
                        .frame(maxWidth: .infinity).padding(.top, 20)
                }
            }
            .padding(10)
        }
    }
}

// MARK: - Layer row

struct LayerRow: View {
    @ObservedObject var doc: DocStore
    let layer: DocLayer
    let selected: Bool

    var body: some View {
        HStack(spacing: 7) {
            Image(systemName: layer.visible ? "eye" : "eye.slash")
                .font(.system(size: 9))
                .foregroundStyle(layer.visible ? Kou.text2 : Kou.text3)
                .frame(width: 14)
                .onTapGesture {
                    doc.setLayer(layer.id, ["visible": !layer.visible])
                }
            Image(systemName: kKindIcons[layer.kind] ?? "square")
                .font(.system(size: 10))
                .foregroundStyle(Kou.accent)
                .frame(width: 15)
            Text(layer.name)
                .font(.system(size: 11, weight: selected ? .semibold : .regular))
                .foregroundStyle(selected ? Kou.text1 : Kou.text2)
                .lineLimit(1)
            Spacer()
            if layer.hasMask {
                Image(systemName: "rectangle.dashed")
                    .font(.system(size: 8)).foregroundStyle(Kou.text3)
            }
            if layer.blend.lowercased() != "normal" {
                Text(blendLabel(layer.blend))
                    .font(.system(size: 8)).foregroundStyle(Kou.text3)
            }
            if layer.opacity < 1 {
                Text("\(Int(layer.opacity * 100))%")
                    .font(.system(size: 8).monospacedDigit()).foregroundStyle(Kou.text3)
            }
        }
        .padding(.horizontal, 7).padding(.vertical, 5)
        .background(selected ? Kou.accentSoft : Color.clear)
        .clipShape(RoundedRectangle(cornerRadius: 5))
        .overlay(RoundedRectangle(cornerRadius: 5)
            .stroke(selected ? Kou.accent.opacity(0.5) : Color.clear, lineWidth: 1))
    }
}

// MARK: - Inspector

struct LayerInspector: View {
    @ObservedObject var doc: DocStore
    let layer: DocLayer

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            commonPanel
            contentPanel
            maskPanel
        }
    }

    private var commonPanel: some View {
        Panel("Layer") {
            HStack(spacing: 6) {
                Text("Name").font(.system(size: 10.5, weight: .medium))
                    .foregroundStyle(Kou.text2).frame(width: 60, alignment: .leading)
                CommitField(initial: layer.name) {
                    doc.setLayer(layer.id, ["name": $0], then: .reloadImage)
                }
            }
            DebSliderRow("Opacity", value: layer.opacity, range: 0...1, reset: 1) {
                doc.setLayerThrottled(layer.id, ["opacity": $0])
            }
            HStack(spacing: 6) {
                Text("Blend").font(.system(size: 10.5, weight: .medium))
                    .foregroundStyle(Kou.text2).frame(width: 60, alignment: .leading)
                Menu {
                    ForEach(kBlendModes, id: \.key) { m in
                        Button(m.label) { doc.setLayer(layer.id, ["blend": m.key]) }
                    }
                } label: {
                    HStack(spacing: 4) {
                        Text(blendLabel(layer.blend)).font(.system(size: 10.5))
                        Image(systemName: "chevron.down").font(.system(size: 7, weight: .bold))
                    }
                    .foregroundStyle(Kou.text1)
                    .padding(.horizontal, 7).padding(.vertical, 4)
                    .background(Kou.bg3).clipShape(RoundedRectangle(cornerRadius: 4))
                }
                .menuStyle(.borderlessButton).fixedSize()
                Spacer()
            }
            HStack(spacing: 6) {
                numField("X", layer.x) { doc.setLayer(layer.id, ["x": Int($0)]) }
                numField("Y", layer.y) { doc.setLayer(layer.id, ["y": Int($0)]) }
                numField("Scale", layer.scale) { doc.setLayer(layer.id, ["scale": $0]) }
            }
        }
    }

    private func numField(_ title: String, _ v: Double,
                          _ set: @escaping (Double) -> Void) -> some View {
        HStack(spacing: 3) {
            Text(title).font(.system(size: 9)).foregroundStyle(Kou.text3)
            TextField(title, value: Binding(get: { v }, set: set),
                      format: .number.precision(.fractionLength(0...1)))
            .textFieldStyle(.plain)
            .font(.system(size: 10).monospacedDigit()).foregroundStyle(Kou.text1)
            .frame(width: 52)
            .padding(.horizontal, 5).padding(.vertical, 3)
            .background(Kou.bg3).clipShape(RoundedRectangle(cornerRadius: 4))
        }
    }

    @ViewBuilder private var contentPanel: some View {
        switch layer.kind {
        case "develop", "adjustment":
            RecipeInspector(layer: layer, recipe: layer.recipe ?? Recipe()) { r in
                doc.setRecipe(layer.id, r)
            }
        case "text":
            TextInspector(doc: doc, layer: layer)
        case "fill":
            FillInspector(doc: doc, layer: layer)
        case "shape":
            ShapeInspector(doc: doc, layer: layer)
        case "raster":
            Panel("Source") {
                Text("Raster layer — pixels embedded or linked.")
                    .font(.system(size: 10)).foregroundStyle(Kou.text3)
            }
        default:
            EmptyView()
        }
    }

    private var maskPanel: some View {
        Panel("Mask") {
            HStack(spacing: 8) {
                ToolChip(label: doc.maskArmed ? "Painting" : "Paint mask",
                         icon: "paintbrush", active: doc.maskArmed) {
                    doc.maskArmed.toggle()
                }
                if layer.hasMask {
                    ToolChip(label: "Clear", icon: "xmark") {
                        doc.setLayer(layer.id, ["mask": NSNull()])
                    }
                }
                Spacer()
            }
            if doc.maskArmed {
                SliderRow("Radius", $doc.brushRadius, 4...400, reset: 48)
                SegPicker([(true, "Hide"), (false, "Reveal")], selection: $doc.brushErase)
                Text("Drag on the canvas to paint the mask.")
                    .font(.system(size: 9)).foregroundStyle(Kou.text3)
            }
        }
    }
}

// MARK: - recipe inspector (develop / adjustment layers)

struct RecipeInspector: View {
    let layer: DocLayer
    @State var recipe: Recipe
    var onChange: (Recipe) -> Void
    @State private var debounce: Task<Void, Never>?

    init(layer: DocLayer, recipe: Recipe, onChange: @escaping (Recipe) -> Void) {
        self.layer = layer
        self._recipe = State(initialValue: recipe)
        self.onChange = onChange
    }

    private func push() {
        debounce?.cancel()
        let r = recipe
        debounce = Task { @MainActor in
            try? await Task.sleep(nanoseconds: 160_000_000)
            guard !Task.isCancelled else { return }
            onChange(r)
        }
    }

    private func b(_ kp: WritableKeyPath<Recipe, Double>) -> Binding<Double> {
        Binding(get: { recipe[keyPath: kp] },
                set: { recipe[keyPath: kp] = $0; push() })
    }

    var body: some View {
        Panel(layer.kind == "develop" ? "Develop" : "Adjustment") {
            if layer.kind == "adjustment" {
                Text("Crop/rotate/keystone are ignored on adjustment layers.")
                    .font(.system(size: 9)).foregroundStyle(Kou.text3)
            }
            SegPicker([(WbMode.asShot, "As Shot"), (.auto, "Auto"),
                       (.manual, "Manual")],
                      selection: Binding(get: { recipe.wb_mode },
                                         set: { recipe.wb_mode = $0; push() }))
            SliderRow("Temp", b(\.temperature), -1...1, track: Kou.tempTrack)
            SliderRow("Tint", b(\.tint), -1...1, track: Kou.tintTrack)
        }
        Panel("Tone") {
            SliderRow("Exposure", b(\.exposure), -4...4, step: 0.05)
            SliderRow("Contrast", b(\.contrast), -1...1)
            SliderRow("Highlights", b(\.highlights), -1...1)
            SliderRow("Shadows", b(\.shadows), -1...1)
            SliderRow("Whites", b(\.whites), -1...1)
            SliderRow("Blacks", b(\.blacks), -1...1)
        }
        Panel("Color") {
            SliderRow("Saturation", b(\.saturation), -1...1)
            SliderRow("Vibrance", b(\.vibrance), -1...1)
            SliderRow("Shd Sat", b(\.shadow_sat), 0...1)
            SliderRow("Hi Sat", b(\.highlight_sat), 0...1)
        }
        Panel("Detail") {
            SliderRow("Sharpen", b(\.sharpen), 0...2)
            SliderRow("NR Luma", b(\.noise_luma), 0...1)
            SliderRow("NR Chroma", b(\.noise_chroma), 0...1)
            SliderRow("Clarity", b(\.clarity), -1...1)
        }
        Panel("Effects") {
            SliderRow("Vignette", b(\.vignette), -1...1)
            SliderRow("Grain", b(\.grain), 0...1)
            SliderRow("Glow", b(\.glow), 0...1)
            SliderRow("Beauty", b(\.beauty), 0...1)
        }
    }
}

// MARK: - text inspector

struct TextInspector: View {
    @ObservedObject var doc: DocStore
    let layer: DocLayer

    private var t: [String: Any] { layer.text ?? [:] }

    private func set(_ key: String, _ v: Any, throttle: Bool = false) {
        var nt = t
        nt[key] = v
        if throttle {
            doc.setLayerThrottled(layer.id, ["text": nt])
        } else {
            doc.setLayer(layer.id, ["text": nt], then: .reloadImage)
        }
    }

    private func num(_ k: String, _ def: Double) -> Double {
        (t[k] as? NSNumber)?.doubleValue ?? def
    }

    private func colorComp(_ i: Int) -> Binding<Double> {
        Binding(get: {
            ((t["color"] as? [Any])?.compactMap { ($0 as? NSNumber)?.doubleValue }[safe: i]) ?? 1
        }, set: { nv in
            var c = (t["color"] as? [Any])?.compactMap { ($0 as? NSNumber)?.doubleValue }
                ?? [1, 1, 1, 1]
            while c.count < 4 { c.append(1) }
            c[i] = nv
            var nt = t; nt["color"] = c
            doc.setLayerThrottled(layer.id, ["text": nt])
        })
    }

    var body: some View {
        Panel("Text") {
            HStack(spacing: 6) {
                Text("Content").font(.system(size: 10.5, weight: .medium))
                    .foregroundStyle(Kou.text2).frame(width: 60, alignment: .leading)
                CommitField(initial: t["text"] as? String ?? "") { set("text", $0) }
            }
            DebSliderRow("Size", value: num("size", 64), range: 4...1000, reset: 64) {
                set("size", $0, throttle: true)
            }
            HStack(spacing: 6) {
                Text("Font").font(.system(size: 10.5, weight: .medium))
                    .foregroundStyle(Kou.text2).frame(width: 60, alignment: .leading)
                CommitField(initial: t["font"] as? String ?? "",
                            placeholder: "Font name (empty = system)") { set("font", $0) }
            }
            HStack(spacing: 6) {
                Text("Align").font(.system(size: 10.5, weight: .medium))
                    .foregroundStyle(Kou.text2).frame(width: 60, alignment: .leading)
                SegPicker([("left", "L"), ("center", "C"), ("right", "R")],
                          selection: Binding(
                            get: { (t["align"] as? String ?? "left").lowercased() },
                            set: { set("align", $0) }))
                Toggle("B", isOn: Binding(
                    get: { t["bold"] as? Bool ?? false },
                    set: { set("bold", $0) }))
                .toggleStyle(.button).controlSize(.mini)
                Toggle("I", isOn: Binding(
                    get: { t["italic"] as? Bool ?? false },
                    set: { set("italic", $0) }))
                .toggleStyle(.button).controlSize(.mini)
            }
            DebSliderRow("Tracking", value: num("tracking", 0), range: -0.2...1) {
                set("tracking", $0, throttle: true)
            }
            DebSliderRow("Leading", value: num("leading", 1.2), range: 0.5...3, reset: 1.2) {
                set("leading", $0, throttle: true)
            }
            DebSliderRow("R", value: colorVal(0), range: 0...1, reset: 1) { setColor(0, $0) }
            DebSliderRow("G", value: colorVal(1), range: 0...1, reset: 1) { setColor(1, $0) }
            DebSliderRow("B", value: colorVal(2), range: 0...1, reset: 1) { setColor(2, $0) }
            DebSliderRow("A", value: colorVal(3), range: 0...1, reset: 1) { setColor(3, $0) }
        }
    }

    private func colorVal(_ i: Int) -> Double {
        ((t["color"] as? [Any])?.compactMap { ($0 as? NSNumber)?.doubleValue }[safe: i]) ?? 1
    }

    private func setColor(_ i: Int, _ nv: Double) {
        var c = (t["color"] as? [Any])?.compactMap { ($0 as? NSNumber)?.doubleValue }
            ?? [1, 1, 1, 1]
        while c.count < 4 { c.append(1) }
        c[i] = nv
        set("color", c, throttle: true)
    }
}

// MARK: - fill inspector

struct FillInspector: View {
    @ObservedObject var doc: DocStore
    let layer: DocLayer

    private var fillColor: [Double] {
        let c = (layer.fill?["color"] as? [Any])?
            .compactMap { ($0 as? NSNumber)?.doubleValue } ?? [0.5, 0.5, 0.5, 1]
        return c
    }

    private func setColor(_ i: Int?, _ nv: Double) {
        var c = fillColor
        while c.count < 4 { c.append(1) }
        if let i { c[i] = nv }
        let nt: [String: Any] = ["kind": "solid", "color": c]
        doc.setLayerThrottled(layer.id, ["fill": nt])
    }

    private func setSwatch(_ c: [Double]) {
        doc.setLayer(layer.id, ["fill": ["kind": "solid", "color": c]])
    }

    var body: some View {
        Panel("Fill") {
            HStack(spacing: 6) {
                ForEach([[1,1,1,1],[0,0,0,1],[0.96,0.66,0.24,1],[0.85,0.25,0.25,1],
                         [0.25,0.6,0.35,1],[0.25,0.45,0.9,1],[0.6,0.35,0.8,1]] as [[Double]],
                        id: \.self) { c in
                    Circle()
                        .fill(Color(red: c[0], green: c[1], blue: c[2]).opacity(c[3]))
                        .frame(width: 16, height: 16)
                        .overlay(Circle().stroke(Kou.border, lineWidth: 1))
                        .onTapGesture { setSwatch(c) }
                }
                Spacer()
            }
            DebSliderRow("R", value: fillColor[safe: 0] ?? 0.5, range: 0...1) { setColor(0, $0) }
            DebSliderRow("G", value: fillColor[safe: 1] ?? 0.5, range: 0...1) { setColor(1, $0) }
            DebSliderRow("B", value: fillColor[safe: 2] ?? 0.5, range: 0...1) { setColor(2, $0) }
            DebSliderRow("A", value: fillColor[safe: 3] ?? 1, range: 0...1) { setColor(3, $0) }
            Text("Solid colour; swatches apply instantly.")
                .font(.system(size: 9)).foregroundStyle(Kou.text3)
        }
    }
}

// MARK: - shape inspector

struct ShapeInspector: View {
    @ObservedObject var doc: DocStore
    let layer: DocLayer
    @State private var dText = ""

    private func setShapes(_ shapes: [[String: Any]]) {
        doc.setLayer(layer.id, ["shapes": shapes], then: .reloadImage)
    }

    var body: some View {
        Panel("Shape") {
            HStack(spacing: 6) {
                ForEach([("Rect", "rect"), ("Ellipse", "ellipse"), ("Line", "line")],
                        id: \.1) { name, k in
                    ToolChip(label: name, icon: "plus") { addPreset(k) }
                }
                Spacer()
            }
            Text("Path (SVG d)")
                .font(.system(size: 9)).foregroundStyle(Kou.text3)
            TextEditor(text: $dText)
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(Kou.text1)
                .scrollContentBackground(.hidden)
                .background(Kou.bg3)
                .frame(height: 60)
                .clipShape(RoundedRectangle(cornerRadius: 4))
                .onAppear {
                    if dText.isEmpty {
                        dText = layer.shapes?.first?["d"] as? String ?? ""
                    }
                }
            Button("Apply path") {
                setShapes([["d": dText, "fill": [0.96, 0.66, 0.24, 1]]])
            }
            .buttonStyle(KouSecondaryButton())
        }
    }

    private func addPreset(_ kind: String) {
        let w = Double(doc.docW), h = Double(doc.docH)
        let (cx, cy) = (w / 2, h / 2)
        let (rw, rh) = (w / 4, h / 4)
        let d: String
        var shape: [String: Any] = ["fill": [0.96, 0.66, 0.24, 1]]
        switch kind {
        case "rect":
            d = "M \(cx-rw/2) \(cy-rh/2) L \(cx+rw/2) \(cy-rh/2) L \(cx+rw/2) \(cy+rh/2) L \(cx-rw/2) \(cy+rh/2) Z"
        case "ellipse":
            let rx = rw/2, ry = rh/2, kappa = 0.5523
            d = "M \(cx-rx) \(cy) C \(cx-rx) \(cy-ry*kappa) \(cx-rx*kappa) \(cy-ry) \(cx) \(cy-ry) "
              + "C \(cx+rx*kappa) \(cy-ry) \(cx+rx) \(cy-ry*kappa) \(cx+rx) \(cy) "
              + "C \(cx+rx) \(cy+ry*kappa) \(cx+rx*kappa) \(cy+ry) \(cx) \(cy+ry) "
              + "C \(cx-rx*kappa) \(cy+ry) \(cx-rx) \(cy+ry*kappa) \(cx-rx) \(cy) Z"
        default:
            d = "M \(cx-rw/2) \(cy) L \(cx+rw/2) \(cy)"
            shape = ["stroke": ["color": [1.0, 1.0, 1.0, 1.0], "width": 4.0]]
        }
        shape["d"] = d
        setShapes([shape])
    }
}

// MARK: - helpers

/// Text field that commits on Return/focus-loss instead of every keystroke.
struct CommitField: View {
    let initial: String
    var placeholder: String = "Name"
    var onCommit: (String) -> Void
    @State private var text: String = ""

    init(initial: String, placeholder: String = "Name",
         onCommit: @escaping (String) -> Void) {
        self.initial = initial
        self.placeholder = placeholder
        self.onCommit = onCommit
    }

    var body: some View {
        TextField(placeholder, text: $text)
            .textFieldStyle(.plain)
            .font(.system(size: 11)).foregroundStyle(Kou.text1)
            .padding(.horizontal, 6).padding(.vertical, 4)
            .background(Kou.bg3).clipShape(RoundedRectangle(cornerRadius: 4))
            .onAppear { text = initial }
            .onSubmit { onCommit(text) }
    }
}

/// SliderRow that keeps a local value while dragging and commits through a
/// throttled callback — one dispatch per gesture, not per frame.
struct DebSliderRow: View {
    let title: String
    let value: Double
    let range: ClosedRange<Double>
    var reset: Double? = nil
    var onChange: (Double) -> Void
    @State private var local: Double = 0
    @State private var seq = 0

    init(_ title: String, value: Double, range: ClosedRange<Double>,
         reset: Double? = nil, onChange: @escaping (Double) -> Void) {
        self.title = title; self.value = value; self.range = range
        self.reset = reset; self.onChange = onChange
    }

    var body: some View {
        SliderRow(title, Binding(
            get: { local },
            set: { nv in
                local = nv
                seq += 1
                let n = seq
                Task { @MainActor in
                    try? await Task.sleep(nanoseconds: 130_000_000)
                    guard n == seq else { return }
                    onChange(nv)
                }
            }), range, reset: reset)
            .onAppear { local = value }
            .onChange(of: value) { _, nv in local = nv }
    }
}

// MARK: - checkerboard

struct Checkerboard: View {
    var body: some View {
        Canvas { ctx, size in
            let s: CGFloat = 10
            let a = Color(white: 0.16), b = Color(white: 0.22)
            ctx.fill(Path(CGRect(origin: .zero, size: size)), with: .color(a))
            var y: CGFloat = 0
            var row = 0
            while y < size.height {
                var x: CGFloat = 0
                var col = row
                while x < size.width {
                    if col % 2 == 0 {
                        ctx.fill(Path(CGRect(x: x, y: y, width: s, height: s)),
                                 with: .color(b))
                    }
                    x += s; col += 1
                }
                y += s; row += 1
            }
        }
    }
}

private extension Array {
    subscript(safe i: Int) -> Element? {
        indices.contains(i) ? self[i] : nil
    }
}
