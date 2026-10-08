import SwiftUI
import AppKit
import QuartzCore
import PDFKit
import UniformTypeIdentifiers

// MARK: - Canvas overlay (separate NSWindow)

/// SwiftUI defers every view update made inside a gesture — even
/// @GestureState and AppKit `setNeedsDisplay` draws — until the mouse
/// comes up: during event tracking the app's render pipeline pauses and
/// window-surface *content* commits sit in the outbound queue. What DOES
/// reach the WindowServer immediately are window-level ops: orderFront/
/// orderOut/setFrame. So the marquee band and the drag outline are each
/// a small borderless NSWindow with static styling (backgroundColor +
/// a CALayer border committed at creation); mid-drag we only move and
/// show/hide them — pure server-side operations that composite live.
/// `CanvasOverlayNSView` is the invisible anchor filling the canvas that
/// converts view-space rects to screen space and owns the windows.
final class CanvasOverlayNSView: NSView {
    private var bandWin: NSWindow?
    private var outlineWin: NSWindow?
    private var monitors: [Any] = []
    /// scroll-wheel on the canvas: (view-space point, delta, modifiers,
    /// view size) — two-finger scroll pans, cmd-scroll zooms. Called
    /// from an NSEvent local monitor so no hit-testing is disturbed.
    var onScroll: ((CGPoint, CGSize, NSEvent.ModifierFlags, CGSize) -> Void)?
    /// spacebar held state for pan mode (keyCode 49)
    var onSpace: ((Bool) -> Void)?
    var onEscape: (() -> Void)?
    private static let accent =
        NSColor(srgbRed: 1.0, green: 0.62, blue: 0.13, alpha: 1)

    /// match SwiftUI's top-left origin — callers pass view-space rects
    override var isFlipped: Bool { true }

    override func hitTest(_ p: NSPoint) -> NSView? { nil }

    private func installMonitors() {
        for m in monitors { NSEvent.removeMonitor(m) }
        monitors.removeAll()
        if let m = NSEvent.addLocalMonitorForEvents(
            matching: .scrollWheel,
            handler: { [weak self] ev in
                guard let self = self, let w = self.window,
                      ev.window === w else { return ev }
                let p = self.convert(ev.locationInWindow, to: nil)
                if self.bounds.contains(p) {
                    self.onScroll?(p, CGSize(width: ev.deltaX, height: ev.deltaY),
                                   ev.modifierFlags, self.bounds.size)
                }
                return ev
            }) { monitors.append(m) }
        for mask: NSEvent.EventTypeMask in [.keyDown, .keyUp] {
            if let m = NSEvent.addLocalMonitorForEvents(
                matching: mask,
                handler: { [weak self] ev in
                    guard let self = self, ev.window === self.window,
                          !ev.isARepeat else { return ev }
                    if ev.keyCode == 49 {
                        self.onSpace?(mask == .keyDown)
                    } else if ev.keyCode == 53, mask == .keyDown {
                        self.onEscape?()
                    }
                    return ev
                }) { monitors.append(m) }
        }
    }

    private func makeWindow(fill: NSColor?, border: NSColor?) -> NSWindow {
        let ow = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 4, height: 4),
                          styleMask: .borderless,
                          backing: .buffered, defer: false)
        ow.backgroundColor = fill ?? .clear
        ow.isOpaque = false
        ow.hasShadow = false
        ow.ignoresMouseEvents = true
        ow.isReleasedWhenClosed = false
        ow.level = .floating
        let v = NSView()
        v.wantsLayer = true
        v.layer?.borderWidth = border == nil ? 0 : 1
        v.layer?.borderColor = border?.cgColor
        ow.contentView = v
        return ow
    }

    /// place/hide one overlay window for a view-coords rect
    private func place(_ win: inout NSWindow?, viewRect r: CGRect?,
                       fill: NSColor?, border: NSColor?) {
        if let r = r, r.width > 0.5, r.height > 0.5, let w = window {
            let sr = w.convertToScreen(convert(r, to: nil))
            if win == nil { win = makeWindow(fill: fill, border: border) }
            win?.setFrame(sr, display: false)
            win?.orderFront(nil)
        } else {
            win?.orderOut(nil)
        }
    }

    /// update the marquee band / moving outline; rects are view coords.
    func setDraw(band: CGRect?, outline: CGRect?) {
        place(&bandWin, viewRect: band,
              fill: Self.accent.withAlphaComponent(0.3),
              border: Self.accent)
        place(&outlineWin, viewRect: outline,
              fill: nil,
              border: Self.accent.withAlphaComponent(0.9))
    }

    /// standalone windows outlive gesture state: if the canvas window
    /// closes, miniaturises or the app resigns active while a band is up,
    /// it would otherwise float forever — always hide on those transitions.
    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        NotificationCenter.default.removeObserver(self)
        if window != nil { installMonitors() }
        else {
            for m in monitors { NSEvent.removeMonitor(m) }
            monitors.removeAll()
        }
        guard let w = window else { return }
        let hide: (Notification) -> Void = { [weak self] _ in
            self?.setDraw(band: nil, outline: nil)
        }
        NotificationCenter.default.addObserver(
            forName: NSWindow.willCloseNotification, object: w, queue: .main,
            using: hide)
        NotificationCenter.default.addObserver(
            forName: NSWindow.didMiniaturizeNotification, object: w, queue: .main,
            using: hide)
        NotificationCenter.default.addObserver(
            forName: NSApplication.didResignActiveNotification, object: nil,
            queue: .main, using: hide)
    }

    deinit {
        for m in monitors { NSEvent.removeMonitor(m) }
        bandWin?.orderOut(nil); outlineWin?.orderOut(nil)
        NotificationCenter.default.removeObserver(self)
    }
}

/// bridge: the representable lives in the ZStack; the store holds a
/// weak ref so gesture callbacks can push rects straight to AppKit.
private struct CanvasOverlay: NSViewRepresentable {
    var doc: DocStore
    func makeNSView(context: Context) -> CanvasOverlayNSView {
        let v = CanvasOverlayNSView()
        doc.overlay = v
        v.onScroll = { [weak doc] p, d, mods, size in
            doc?.canvasScroll(at: p, delta: d, mods: mods, viewSize: size)
        }
        v.onSpace = { [weak doc] held in doc?.spaceHeld = held }
        // Esc: exit node-edit (SwiftUI .onExitCommand never fires — the
        // canvas NSView is first responder and eats the keyDown)
        v.onEscape = { [weak doc] in
            if doc?.nodeEdit == true {
                doc?.nodeEdit = false
                doc?.refreshNodes()
            }
        }
        return v
    }
    func updateNSView(_ v: CanvasOverlayNSView, context: Context) {}
}

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
    var children: [DocLayer] = []
    /// {"dropShadow": {dx,dy,blur,spread,color:[r,g,b,a]}} or absent
    var styles: [String: Any]?

    static func == (a: DocLayer, b: DocLayer) -> Bool {
        a.id == b.id && a.name == b.name && a.kind == b.kind
            && a.visible == b.visible && a.opacity == b.opacity
            && a.blend == b.blend && a.hasMask == b.hasMask
            && a.x == b.x && a.y == b.y && a.scale == b.scale
            && a.children.count == b.children.count
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
    /// multi-select set from marquee/cmd-click — `selected` stays the primary
    @Published var selectedSet: Set<UInt64> = []
    /// canvas-visual state that must outlive gestures — the in-flight
    /// drag visuals use @GestureState on the view instead.
    @Published var hoverPt: CGPoint = .zero     // brush cursor position
    /// on-canvas text editor: (layer id, draft)
    @Published var textEdit: (id: UInt64, text: String)? = nil
    /// AppKit overlay — repaints instantly mid-gesture, unlike SwiftUI
    /// state which defers all view updates inside an HSplitView child
    /// until the mouse comes up.
    weak var overlay: CanvasOverlayNSView?
    /// placed bounds in doc coords for every top-level layer (doc.bounds)
    @Published var layerBounds: [UInt64: CGRect] = [:]
    /// collapsed group ids in the layers panel
    @Published var collapsed: Set<UInt64> = []
    /// layer id currently being dragged in the layers panel
    var draggingId: UInt64?
    /// drop indicator: (target row id, edge, target's parent)
    @Published var dropEdge: (id: UInt64, edge: DropEdge, parent: UInt64?)?
    @Published var busy = false
    @Published var dirty = false
    @Published var error: String?
    @Published var docPath: String?
    // mask painting
    @Published var maskArmed = false
    @Published var brushRadius: Double = 48
    @Published var brushErase = true   // true = hide, false = reveal
    @Published var brushSoftness: Double = 0.6
    // canvas view transform: zoom (1 = fit) + pan in view points
    @Published var zoom: CGFloat = 1
    @Published var pan: CGSize = .zero
    /// spacebar held → drag pans instead of moving layers
    @Published var spaceHeld = false
    /// node-edit mode for the selected shape layer
    @Published var nodeEdit = false
    let previewMaxPx: UInt32 = 1600

    private var pendingImageReload = false

    /// recursive lookup — the inspector can edit nested group children
    var selLayer: DocLayer? { selected.flatMap { Self.find($0, in: layers) } }

    static func find(_ id: UInt64, in list: [DocLayer]) -> DocLayer? {
        for l in list {
            if l.id == id { return l }
            if let f = find(id, in: l.children) { return f }
        }
        return nil
    }

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

    /// Export the composite. format: png|jpeg|tiff|psd
    func exportAs(_ format: String) {
        let p = NSSavePanel()
        let ext = format == "jpeg" ? "jpg" : format
        if let t = UTType(filenameExtension: ext) { p.allowedContentTypes = [t] }
        p.nameFieldStringValue = (docName.isEmpty ? "Untitled" : docName) + "." + ext
        guard p.runModal() == .OK, let url = p.url else { return }
        if format == "psd" {
            dispatch(["id": "doc.exportPsd", "path": url.path], then: .none)
        } else {
            dispatch(["id": "doc.render", "out": url.path,
                      "format": format, "quality": 92], then: .none)
        }
    }

    // ---- canvas tools ----

    /// engine-side alpha-precise hit test at a document point
    func pick(at pt: CGPoint, then hit: @escaping (UInt64?) -> Void) {
        guard let s = ensure() else { return }
        Task.detached { [weak self] in
            let r = await s.work { sess in
                sess.dispatch(["id": "doc.pick", "x": Double(pt.x), "y": Double(pt.y)])
            }
            let lid = (r["result"] as? [String: Any])?["layer"] as? NSNumber
            await MainActor.run { hit(lid?.uint64Value) }
            _ = self
        }
    }

    /// synchronous pick for gestures — the hit test is microseconds; an
    /// async round-trip loses any drag released before the answer lands
    func pickSync(at pt: CGPoint) -> UInt64? {
        guard let s = ensure() else { return nil }
        let r = s.workSync { sess in
            sess.dispatch(["id": "doc.pick", "x": Double(pt.x), "y": Double(pt.y)])
        }
        return ((r["result"] as? [String: Any])?["layer"] as? NSNumber)?.uint64Value
    }

    /// client-side bounds hit test — instant, used for hover/outline only;
    /// real drags go through `pick` (alpha+mask aware)
    func layerAtBounds(_ pt: CGPoint) -> UInt64? {
        for l in layers.reversed() where l.kind != "adjustment" {
            if let b = layerBounds[l.id], b.contains(pt) { return l.id }
        }
        return nil
    }

    /// drag-move: absolute target for a layer's x/y (doc coords).
    /// Optimistically updates parsed state + stored bounds so the
    /// inspector/overlay stay in sync; the engine commit is throttled.
    func moveLayerTo(_ id: UInt64, x: Double, y: Double) {
        var d = CGPoint.zero
        _ = Self.mutate(id, in: &layers) { l in
            d = CGPoint(x: x - l.x, y: y - l.y)
            l.x = x; l.y = y
        }
        if d != .zero, let b = layerBounds[id] {
            layerBounds[id] = b.offsetBy(dx: d.x, dy: d.y)
        }
        setLayerThrottled(id, ["x": Int(x.rounded()), "y": Int(y.rounded())])
    }

    /// optimistic scale update for the scale-handle drag
    func scaleLayerTo(_ id: UInt64, scale: Double) {
        _ = Self.mutate(id, in: &layers) { l in l.scale = scale }
        setLayerThrottled(id, ["scale": scale])
    }

    /// final commit at drag end: force the pending transform through
    /// immediately (bypasses throttle), then re-fetch bounds + image.
    func commitTransform(_ id: UInt64) {
        throttleSeq += 1   // cancel the pending throttled write
        guard let l = Self.find(id, in: layers) else { return }
        setLayer(id, ["x": Int(l.x.rounded()), "y": Int(l.y.rounded()),
                      "scale": l.scale], then: .reloadImage)
        refreshBounds()
    }

    /// re-fetch per-layer bounds only (used after transforms)
    func refreshBounds() {
        guard let s = ensure() else { return }
        Task.detached { [weak self] in
            let rs = await s.work { sess in
                sess.dispatch(["id": "doc.bounds"])
            }
            let rows = (rs["result"] as? [[String: Any]]) ?? []
            var map: [UInt64: CGRect] = [:]
            for r in rows {
                if let lid = (r["layer"] as? NSNumber)?.uint64Value,
                   let bb = r["bounds"] as? [NSNumber], bb.count == 4 {
                    map[lid] = CGRect(x: bb[0].doubleValue, y: bb[1].doubleValue,
                                      width: bb[2].doubleValue, height: bb[3].doubleValue)
                }
            }
            await MainActor.run { self?.layerBounds = map }
        }
    }

    /// recursive in-place mutation helper for nested layer arrays
    @discardableResult
    static func mutate(_ id: UInt64, in list: inout [DocLayer],
                       _ f: (inout DocLayer) -> Void) -> Bool {
        for i in list.indices {
            if list[i].id == id { f(&list[i]); return true }
            if mutate(id, in: &list[i].children, f) { return true }
        }
        return false
    }

    func groupSelection() {
        var ids = selectedSet.filter { $0 != 0 }
        if ids.count < 2, let s = selected { ids = [s] }
        guard !ids.isEmpty else { return }
        dispatch(["id": "doc.group", "layers": ids.map { $0 }, "name": "Group"])
        selectedSet = []
    }

    func ungroup(_ id: UInt64) {
        dispatch(["id": "doc.ungroup", "layer": id])
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

    /// Batch: doc.json + bounds + preview render in one queue hop.
    func reloadState() {
        guard let s = session else { return }
        Task.detached { [weak self] in
            let (doc, img, bounds) = await s.work { sess -> ([String: Any], CGImage?, [UInt64: CGRect]) in
                let dj = sess.dispatch(["id": "doc.json"])["result"] as? [String: Any] ?? [:]
                let img = Self.decodePNG(sess.dispatch(
                    ["id": "doc.render", "maxPx": self?.previewMaxPx ?? 1600]))
                var bmap: [UInt64: CGRect] = [:]
                if let arr = sess.dispatch(["id": "doc.bounds"])["result"] as? [[String: Any]] {
                    for e in arr {
                        if let id = (e["layer"] as? NSNumber)?.uint64Value,
                           let b = e["bounds"] as? [NSNumber], b.count == 4 {
                            bmap[id] = CGRect(x: b[0].doubleValue, y: b[1].doubleValue,
                                              width: b[2].doubleValue, height: b[3].doubleValue)
                        }
                    }
                }
                return (dj, img, bmap)
            }
            await MainActor.run {
                self?.layerBounds = bounds
                self?.applyState(doc, img)
            }
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
        if let sel = selected, Self.find(sel, in: layers) == nil {
            selected = layers.last?.id
        }
        selectedSet = selectedSet.filter { Self.find($0, in: layers) != nil }
        if nodeEdit && selLayer?.kind != "shape" { nodeEdit = false }
        if nodeEdit { refreshNodes() }
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
        l.children = (d["children"] as? [[String: Any]] ?? [])
            .compactMap { parseLayer($0) }
        l.styles = d["styles"] as? [String: Any]
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

    /// reparent/reorder via `doc.moveLayer`: parent nil = top level.
    /// `to` is the index inside the parent list (0 = bottom).
    func moveLayer(_ id: UInt64, parent: UInt64?, to: Int) {
        var c: [String: Any] = ["id": "doc.moveLayer", "layer": id, "to": to]
        if let p = parent { c["parent"] = p }
        dispatch(c)
    }

    func setRecipe(_ id: UInt64, _ recipe: Recipe) {
        guard let data = try? JSONEncoder().encode(recipe),
              let obj = try? JSONSerialization.jsonObject(with: data)
        else { return }
        dispatch(["id": "doc.setLayer", "layer": id, "recipe": obj])
    }

    // ---- canvas view transform (zoom/pan) ----

    /// zoom in/out by a factor about the view center (pan preserved)
    func zoomBy(_ f: CGFloat) {
        zoom = min(max(zoom * f, 0.05), 64)
    }
    /// ⌘0: fit the document (reset zoom + pan); ⌘1: 100% pixels
    func zoomFit() { zoom = 1; pan = .zero }
    func zoom100() { zoom = 1 }   // same as fit here — fit *is* 100% of canvas

    /// scroll-wheel on the canvas: cmd-scroll zooms about the cursor,
    /// plain scroll pans (natural scrolling: content follows fingers)
    func canvasScroll(at p: CGPoint, delta d: CGSize,
                      mods: NSEvent.ModifierFlags, viewSize size: CGSize) {
        if mods.contains(.command) {
            let z0 = zoom
            let z1 = min(max(z0 * exp(d.height * 0.004), 0.05), 64)
            guard z1 != z0 else { return }
            // keep the doc point under the cursor fixed
            let f = z1 / z0
            pan = CGSize(
                width: p.x - size.width / 2 - (p.x - size.width / 2 - pan.width) * f,
                height: p.y - size.height / 2 - (p.y - size.height / 2 - pan.height) * f)
            zoom = z1
        } else {
            pan = CGSize(width: pan.width + d.width,
                         height: pan.height + d.height)
        }
    }

    // ---- node editing (shape layers) ----

    /// cached node list for the node-edited layer: [(x, y, kind)]
    @Published var nodes: [[String: Any]] = []
    func refreshNodes() {
        guard nodeEdit, let id = selected,
              DocStore.find(id, in: layers)?.kind == "shape" else {
            nodes = []; return
        }
        guard let s = ensure() else { return }
        let r = s.workSync { sess in
            sess.dispatch(["id": "doc.shapeNodes", "layer": id])
        }
        nodes = (r["result"] as? [String: Any])?["nodes"] as? [[String: Any]] ?? []
    }
    /// move node `index` of the node-edited layer to doc coords (x,y)
    func moveNode(_ index: Int, x: Double, y: Double) {
        guard let id = selected else { return }
        dispatch(["id": "doc.moveNode", "layer": id, "index": index,
                  "x": x, "y": y])
    }

    /// Continuous mask stroke: dispatch the dab immediately (cheap buffer
    /// edit) and coalesce the composite repaint.
    func paintMask(cx: Double, cy: Double) {
        guard let id = selected else { return }
        dispatch(["id": "doc.maskPaint", "layer": id,
                  "cx": cx, "cy": cy, "r": brushRadius,
                  "value": brushErase ? 0.0 : 1.0,
                  "softness": brushSoftness],
                 then: .none)
        reloadImageSoon()
    }

    // ---- vector (vectorcraft) ----

    /// Append a generated shape (rect|roundRect|ellipse|star|line) to the
    /// selected shape layer — or make a shape layer first if needed.
    func addGenShape(_ gen: String) {
        let run: () -> Void = { [weak self] in
            guard let self, let id = self.selected,
                  self.selLayer?.kind == "shape" else { return }
            let params: [String: Any]
            switch gen {
            case "star": params = ["gen": "star", "cx": self.docW / 2,
                                   "cy": self.docH / 2, "r": self.docH / 4,
                                   "points": 5, "innerRatio": 0.45]
            case "line": params = ["gen": "line", "x0": self.docW / 4,
                                   "y0": self.docH / 2, "x1": self.docW * 3 / 4,
                                   "y1": self.docH / 2]
            default: params = ["gen": gen, "x": self.docW / 4, "y": self.docH / 4,
                               "w": self.docW / 2, "h": self.docH / 2,
                               "radius": 24]
            }
            var cmd: [String: Any] = ["id": "doc.addShape", "layer": id,
                                      "fill": [0.96, 0.66, 0.24, 1],
                                      "stroke": ["color": [0, 0, 0, 1], "width": 2]]
            cmd.merge(params) { _, n in n }
            self.dispatch(cmd)
        }
        if selLayer?.kind == "shape" {
            run()
        } else {
            addLayer(kind: "shape")
            // applyState refresh then add — schedule one hop later and
            // force-select the fresh layer (applyState only auto-selects
            // when nothing was selected)
            Task { @MainActor [weak self] in
                try? await Task.sleep(nanoseconds: 400_000_000)
                if let top = self?.layers.last?.id,
                   self?.selLayer?.kind == "shape" {
                    self?.selected = top
                }
                self?.addGenShape(gen)
            }
        }
    }

    // ---- printcraft: PDF → raster layers ----

    /// Import every PDF page as a raster layer (PDFKit render at 2x →
    /// embedded RGBA). Starts a fresh document sized to page 1.
    func importPdf() {
        let p = NSOpenPanel()
        p.allowedContentTypes = [.pdf]
        guard p.runModal() == .OK, let url = p.url,
              let pdf = PDFDocument(url: url),
              let page0 = pdf.page(at: 0) else { return }
        let box = page0.bounds(for: .mediaBox)
        let scale: CGFloat = 2.0
        let W = Int(box.width * scale), H = Int(box.height * scale)
        dispatch(["id": "doc.new", "name": url.deletingPathExtension().lastPathComponent,
                  "w": W, "h": H], then: .none)
        for i in 0..<pdf.pageCount {
            guard let page = pdf.page(at: i),
                  let rgba = Self.renderPdfPage(page, box: box, scale: scale)
            else { continue }
            let b64 = rgba.base64EncodedString()
            dispatch(["id": "doc.addLayer", "kind": "raster",
                      "name": "Page \(i + 1)", "w": W, "h": H,
                      "rgbaB64": b64], then: .none)
        }
        reloadState()
    }

    /// Render one PDFPage into premultiplied RGBA bytes (white backdrop —
    /// pages are opaque paper).
    static func renderPdfPage(_ page: PDFPage, box: CGRect, scale: CGFloat) -> Data? {
        let W = Int(box.width * scale), H = Int(box.height * scale)
        guard let ctx = CGContext(
            data: nil, width: W, height: H, bitsPerComponent: 8,
            bytesPerRow: W * 4, space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return nil }
        ctx.setFillColor(.white)
        ctx.fill(CGRect(x: 0, y: 0, width: W, height: H))
        ctx.translateBy(x: 0, y: CGFloat(H))
        ctx.scaleBy(x: scale, y: -scale)
        page.draw(with: .mediaBox, to: ctx)
        return ctx.data.map { Data(bytes: $0, count: W * H * 4) }
    }
}

extension CGPoint {
    func distance(to o: CGPoint) -> CGFloat {
        let dx = x - o.x, dy = y - o.y
        return (dx * dx + dy * dy).squareRoot()
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
            // zoom controls: ⌘-/⌘+ step, ⌘0 fits (resets pan too)
            HStack(spacing: 2) {
                Button("−") { doc.zoomBy(1 / 1.25) }
                    .buttonStyle(KouSecondaryButton())
                Menu {
                    Button("Fit (⌘0)") { doc.zoomFit() }
                    Button("50%") { doc.zoom = 0.5; doc.pan = .zero }
                    Button("100%") { doc.zoom = 1; doc.pan = .zero }
                    Button("200%") { doc.zoom = 2; doc.pan = .zero }
                    Button("400%") { doc.zoom = 4; doc.pan = .zero }
                } label: {
                    Text("\(Int((doc.zoom * 100).rounded()))%")
                        .font(.system(size: 10, weight: .medium).monospacedDigit())
                        .frame(minWidth: 34)
                }
                .menuStyle(.borderlessButton)
                .fixedSize()
                Button("+") { doc.zoomBy(1.25) }
                    .buttonStyle(KouSecondaryButton())
            }
            .help("Zoom — scroll to pan, ⌘-scroll or pinch to zoom, space-drag to pan")
            Spacer()
            if doc.busy { ProgressView().controlSize(.small).tint(Kou.accent) }
            if let e = doc.error {
                Text(e).font(.system(size: 10)).foregroundStyle(.red.opacity(0.9)).lineLimit(1)
            }
            Button("Save…") { doc.save() }
                .buttonStyle(KouSecondaryButton())
            Menu {
                Button("PNG…") { doc.exportAs("png") }
                Button("JPEG…") { doc.exportAs("jpeg") }
                Button("TIFF…") { doc.exportAs("tiff") }
                Button("PSD (flat)…") { doc.exportAs("psd") }
            } label: {
                Text("Export")
                    .font(.system(size: 11, weight: .semibold))
            }
            .menuStyle(.borderlessButton)
            .fixedSize()
            .padding(.horizontal, 10).padding(.vertical, 5)
            .background(Kou.accent).clipShape(RoundedRectangle(cornerRadius: 5))
            .foregroundStyle(.black)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(Kou.bg1)
        .overlay(alignment: .bottom) { Kou.hairline.frame(height: 1) }
        .background(
            // cmd+S + zoom shortcuts
            VStack {
                Button("") { doc.save() }
                    .keyboardShortcut("s", modifiers: .command)
                Button("") { doc.zoomFit() }
                    .keyboardShortcut("0", modifiers: .command)
                Button("") { doc.zoomBy(1.25) }
                    .keyboardShortcut("=", modifiers: .command)
                Button("") { doc.zoomBy(1 / 1.25) }
                    .keyboardShortcut("-", modifiers: .command)
            }
            .opacity(0).frame(width: 0, height: 0)
        )
    }

    // ---- canvas ----


    /// what an in-progress drag is doing
    private enum DragMode {
        case none
        case moving(UInt64, CGPoint, CGPoint)   // layer, doc-space start, orig x/y
        case marquee(CGPoint, CGPoint)          // start, current (view space)
        case scaling(UInt64, CGPoint, Double, Double) // layer, bbox center, start dist, orig scale
        case panning                            // space held: pan the canvas
        case node(Int, CGPoint)                 // node index, doc-space start
    }
    @State private var dragMode: DragMode = .none
    @State private var pickStarted = false
    /// live pinch/pan deltas — @GestureState updates mid-gesture
    /// (@Published/@State writes inside a gesture defer to release)
    @GestureState private var pinchBy: CGFloat = 1
    @GestureState private var pinchPan: CGSize = .zero
    @GestureState private var dragPan: CGSize = .zero
    /// live node drag position (doc coords) — handle only; the shape
    /// pixels commit on release
    @State private var nodeLive: (index: Int, pt: CGPoint)? = nil
    /// last tap on a text layer — a second tap within 0.45s opens the
    /// on-canvas editor (drag-based: a zero-distance DragGesture always
    /// wins over TapGesture(count:2), so clicks are detected here)
    @State private var lastTap: (time: Date, id: UInt64)? = nil
    /// live gesture visuals — @GestureState updates mid-gesture,
    /// unlike @State/@Published which defer until the gesture ends
    /// live gesture span — drives all in-flight canvas visuals
    /// (state writes inside gesture callbacks defer until release)

    private var canvas: some View {
        GeometryReader { geo in
            let rect = canvasRect(in: geo.size)
            ZStack {
                Checkerboard()
                    .clipShape(RoundedRectangle(cornerRadius: 4))
                if let img = doc.composite {
                    Image(decorative: img, scale: 1)
                        .resizable()
                        .interpolation(.high)
                        .frame(width: rect.width, height: rect.height)
                        .position(x: rect.midX, y: rect.midY)
                } else {
                    ProgressView().tint(Kou.accent)
                        .position(x: geo.size.width / 2, y: geo.size.height / 2)
                }

                selectionOverlay(in: geo.size)
                    .allowsHitTesting(false)
                nodeOverlay(in: geo.size)
                    .allowsHitTesting(false)

                // in-flight drag visuals (marquee band / moving outline)
                CanvasOverlay(doc: doc)
                    .allowsHitTesting(false)

                // on-canvas text editor
                if let ed = doc.textEdit, let b = doc.layerBounds[ed.id] {
                    let vr = viewRect(b, in: geo.size)
                    TextEditor(text: Binding(
                        get: { doc.textEdit?.text ?? "" },
                        set: { doc.textEdit?.text = $0 }))
                        .font(.system(size: max(12, vr.height / 3)))
                        .foregroundStyle(.white)
                        .scrollContentBackground(.hidden)
                        .background(Kou.bg0.opacity(0.75))
                        .overlay(RoundedRectangle(cornerRadius: 4)
                            .stroke(Kou.accent, lineWidth: 1))
                        .frame(width: max(vr.width, 140), height: max(vr.height, 44))
                        .position(x: vr.midX, y: vr.midY)
                        .onExitCommand { commitTextEdit() }
                }

                if doc.maskArmed {
                    Circle()
                        .stroke(Kou.accent, lineWidth: 1.5)
                        .frame(width: brushViewSize(in: geo.size),
                               height: brushViewSize(in: geo.size))
                        .position(doc.hoverPt)
                        .allowsHitTesting(false)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(Kou.bg0)
            .contentShape(Rectangle())
            .onExitCommand {
                if doc.nodeEdit { doc.nodeEdit = false; doc.refreshNodes() }
            }
            .gesture(canvasDrag(in: geo.size))
            .simultaneousGesture(
                MagnifyGesture()
                    .updating($pinchBy) { g, st, _ in
                        st = g.magnification
                    }
                    .updating($pinchPan) { g, st, _ in
                        // keep the pinch anchor's doc point fixed
                        let a = g.startLocation
                        let c = CGPoint(x: geo.size.width / 2 + doc.pan.width,
                                        y: geo.size.height / 2 + doc.pan.height)
                        st = CGSize(
                            width: (a.x - c.x) * (1 - g.magnification),
                            height: (a.y - c.y) * (1 - g.magnification))
                    }
                    .onEnded { g in
                        let a = g.startLocation
                        let c = CGPoint(x: geo.size.width / 2 + doc.pan.width,
                                        y: geo.size.height / 2 + doc.pan.height)
                        doc.pan = CGSize(
                            width: doc.pan.width + (a.x - c.x) * (1 - g.magnification),
                            height: doc.pan.height + (a.y - c.y) * (1 - g.magnification))
                        doc.zoom = min(max(doc.zoom * g.magnification, 0.05), 64)
                    })
            .onContinuousHover { phase in
                switch phase {
                case .active(let p): doc.hoverPt = p
                case .ended: break
                }
            }
        }
        .clipped()
    }

    /// corner handles on the selected layer's bounds + moving outline.
    /// Hit-testing happens in the drag gesture (handles grab first).
    @ViewBuilder
    private func selectionOverlay(in size: CGSize) -> some View {
        if !doc.maskArmed, let sel = doc.selected,
           let b = doc.layerBounds[sel] {
            let vr = viewRect(b, in: size)
            // Photoshop-style transform box: accent border + 4 corner squares
            Rectangle()
                .strokeBorder(Kou.accent.opacity(0.9), lineWidth: 1)
                .frame(width: vr.width, height: vr.height)
                .position(x: vr.midX, y: vr.midY)
            ForEach(0..<4, id: \.self) { corner in
                let p = handlePoint(corner, of: vr)
                RoundedRectangle(cornerRadius: 1.5)
                    .fill(Color.white)
                    .frame(width: 9, height: 9)
                    .overlay(RoundedRectangle(cornerRadius: 1.5)
                        .stroke(Kou.accent, lineWidth: 1))
                    .shadow(color: .black.opacity(0.4), radius: 1)
                    .position(p)
            }
        }
        // extra selected (marquee) layers get a dashed outline
        ForEach(Array(doc.selectedSet.subtracting([doc.selected ?? 0])), id: \.self) { id in
            if let b = doc.layerBounds[id] {
                let vr = viewRect(b, in: size)
                Rectangle()
                    .strokeBorder(Kou.accent.opacity(0.5),
                                  style: StrokeStyle(lineWidth: 1, dash: [3, 3]))
                    .frame(width: vr.width, height: vr.height)
                    .position(x: vr.midX, y: vr.midY)
            }
        }
    }

    /// node handles over a node-edited shape layer
    @ViewBuilder
    private func nodeOverlay(in size: CGSize) -> some View {
        if doc.nodeEdit, let sel = doc.selected,
           DocStore.find(sel, in: doc.layers)?.kind == "shape" {
            ForEach(Array(doc.nodes.enumerated()), id: \.offset) { i, n in
                let nx = (n["x"] as? NSNumber)?.doubleValue ?? 0
                let ny = (n["y"] as? NSNumber)?.doubleValue ?? 0
                let pt = (nodeLive?.index == i ? nodeLive!.pt
                          : CGPoint(x: nx, y: ny))
                let v = viewRect(CGRect(x: pt.x, y: pt.y,
                                        width: 0, height: 0), in: size).origin
                let curved = ["c", "q"].contains((n["kind"] as? String) ?? "")
                ZStack {
                    if curved {
                        RoundedRectangle(cornerRadius: 1.5)
                            .fill(Color.white)
                            .frame(width: 8, height: 8)
                            .overlay(RoundedRectangle(cornerRadius: 1.5)
                                .stroke(Kou.accent, lineWidth: 1))
                    } else {
                        Circle()
                            .fill(Color.white)
                            .frame(width: 7, height: 7)
                            .overlay(Circle()
                                .stroke(Kou.accent, lineWidth: 1))
                    }
                }
                .shadow(color: .black.opacity(0.4), radius: 1)
                .position(v)
            }
        }
    }

    /// corner 0..3 → view-space position of a rect's corner
    private func handlePoint(_ c: Int, of r: CGRect) -> CGPoint {
        switch c {
        case 0: return CGPoint(x: r.minX, y: r.minY)
        case 1: return CGPoint(x: r.maxX, y: r.minY)
        case 2: return CGPoint(x: r.maxX, y: r.maxY)
        default: return CGPoint(x: r.minX, y: r.maxY)
        }
    }

    /// doc-space rect → view-space rect
    private func viewRect(_ docRect: CGRect, in size: CGSize) -> CGRect {
        let rect = canvasRect(in: size)
        guard doc.docW > 0 else { return docRect }
        let k = rect.width / CGFloat(doc.docW)
        return CGRect(x: rect.minX + docRect.minX * k,
                      y: rect.minY + docRect.minY * k,
                      width: docRect.width * k, height: docRect.height * k)
    }

    /// main canvas gesture: pan > node > mask paint > handle > move > marquee
    private func canvasDrag(in size: CGSize) -> some Gesture {
        DragGesture(minimumDistance: 0)
            .updating($dragPan) { g, st, _ in
                if case .panning = dragMode { st = g.translation }
            }
            .onChanged { g in
                let dpt = docPoint(g.location, in: size)
                let startDpt = docPoint(g.startLocation, in: size)
                switch dragMode {
                case .none:
                    if doc.spaceHeld, doc.textEdit == nil {
                        dragMode = .panning
                        return
                    }
                    if doc.maskArmed {
                        doc.paintMask(cx: Double(dpt.x), cy: Double(dpt.y))
                        return
                    }
                    // node-edit mode: grab a node of the selected shape
                    if doc.nodeEdit, let sel = doc.selected,
                       DocStore.find(sel, in: doc.layers)?.kind == "shape" {
                        for (i, n) in doc.nodes.enumerated() {
                            let nx = (n["x"] as? NSNumber)?.doubleValue ?? 0
                            let ny = (n["y"] as? NSNumber)?.doubleValue ?? 0
                            let vp = viewRect(CGRect(x: nx, y: ny,
                                                     width: 0, height: 0),
                                              in: size).origin
                            if vp.distance(to: g.startLocation) <= 10 {
                                dragMode = .node(i, startDpt)
                                return
                            }
                        }
                    }
                    // corner handle grab? (view space, 9pt targets)
                    if let sel = doc.selected, let b = doc.layerBounds[sel] {
                        let vr = viewRect(b, in: size)
                        let pts = (0..<4).map { handlePoint($0, of: vr) }
                        for c in 0..<4 {
                            if pts[c].distance(to: g.startLocation) <= 12 {
                                let center = CGPoint(x: b.midX, y: b.midY)
                                let d0 = startDpt.distance(to: center)
                                let s0 = DocStore.find(sel, in: doc.layers)?.scale ?? 1
                                dragMode = .scaling(sel, center, max(d0, 1), s0)
                                return
                            }
                        }
                    }
                    // else pick once (synchronous — the hit test is
                    // microseconds; async loses fast drags), then decide
                    // move vs marquee
                    if !pickStarted {
                        pickStarted = true
                        if let lid = doc.pickSync(at: startDpt),
                           let l = DocStore.find(lid, in: doc.layers) {
                            dragMode = .moving(lid, startDpt,
                                               CGPoint(x: l.x, y: l.y))
                            doc.selected = lid
                            doc.selectedSet = [lid]
                        } else {
                            dragMode = .marquee(g.startLocation, g.location)
                        }
                        return
                    }
                case .moving(let id, let start, let orig):
                    let dx = dpt.x - start.x, dy = dpt.y - start.y
                    doc.moveLayerTo(id, x: orig.x + dx, y: orig.y + dy)
                    if let b = doc.layerBounds[id] {
                        var vr = viewRect(b, in: size)
                        let k = doc.docW > 0
                            ? canvasRect(in: size).width / CGFloat(doc.docW) : 1
                        vr.origin.x += dx * k
                        vr.origin.y += dy * k
                        doc.overlay?.setDraw(band: nil, outline: vr)
                    }
                case .marquee(let start, _):
                    dragMode = .marquee(start, g.location)
                    let r = CGRect(
                        x: min(start.x, g.location.x),
                        y: min(start.y, g.location.y),
                        width: abs(g.location.x - start.x),
                        height: abs(g.location.y - start.y))
                    doc.overlay?.setDraw(band: r, outline: nil)
                case .scaling(let id, let center, let d0, let s0):
                    let d = dpt.distance(to: center)
                    let ns = s0 * (d / d0)
                    if ns > 0.01, ns < 100 {
                        doc.scaleLayerTo(id, scale: ns)
                        if let b = doc.layerBounds[id] {
                            let ob = CGRect(x: b.minX - (b.width * (ns - 1) / 2),
                                            y: b.minY - (b.height * (ns - 1) / 2),
                                            width: b.width * ns,
                                            height: b.height * ns)
                            doc.overlay?.setDraw(band: nil,
                                                 outline: viewRect(ob, in: size))
                        }
                    }
                case .panning:
                    break   // dragPan @GestureState drives the offset
                case .node(let i, _):
                    nodeLive = (i, dpt)
                }
            }
            .onEnded { g in
                defer {
                    dragMode = .none
                    pickStarted = false
                    doc.overlay?.setDraw(band: nil, outline: nil)
                }
                // tap detection: a gesture that barely moved is a click —
                // two quick clicks on the same text layer open the editor;
                // a tap anywhere else commits an open editor
                let tapLike = g.translation.width * g.translation.width
                    + g.translation.height * g.translation.height < 16
                var opened: UInt64? = nil
                if tapLike, case .moving(let id, _, _) = dragMode,
                   let l = DocStore.find(id, in: doc.layers),
                   l.kind == "text" || l.kind == "shape" {
                    let now = Date()
                    if let last = lastTap, last.id == id,
                       now.timeIntervalSince(last.time)
                        < NSEvent.doubleClickInterval {
                        if l.kind == "text" {
                            doc.textEdit = (id, (l.text?["text"] as? String) ?? "")
                            opened = id
                        } else {
                            doc.nodeEdit.toggle()
                            doc.refreshNodes()
                        }
                        lastTap = nil
                    } else {
                        lastTap = (now, id)
                    }
                } else if tapLike {
                    lastTap = nil
                }
                if doc.textEdit != nil, doc.textEdit?.id != opened {
                    commitTextEdit()
                }
                switch dragMode {
                case .moving(let id, _, let orig):
                    // a bare click is also .moving — commit only when the
                    // transform actually changed, else every selection
                    // click pushes a no-op mutation onto the undo stack
                    if let l = DocStore.find(id, in: doc.layers),
                       l.x != orig.x || l.y != orig.y {
                        doc.commitTransform(id)
                    }
                case .scaling(let id, _, _, let s0):
                    if let l = DocStore.find(id, in: doc.layers),
                       l.scale != s0 {
                        doc.commitTransform(id)
                    }
                case .panning:
                    doc.pan = CGSize(
                        width: doc.pan.width + g.translation.width,
                        height: doc.pan.height + g.translation.height)
                case .node(let i, let start):
                    if let live = nodeLive, live.index == i,
                       doc.nodes.indices.contains(i),
                       let nx = (doc.nodes[i]["x"] as? NSNumber)?.doubleValue,
                       let ny = (doc.nodes[i]["y"] as? NSNumber)?.doubleValue {
                        doc.moveNode(i, x: nx + Double(live.pt.x - start.x),
                                     y: ny + Double(live.pt.y - start.y))
                    }
                    nodeLive = nil
                default:
                    break
                }
                if case .marquee(let a, let b) = dragMode {
                    let mr = CGRect(x: min(a.x, b.x), y: min(a.y, b.y),
                                    width: abs(a.x - b.x),
                                    height: abs(a.y - b.y))
                    let ra = docPoint(mr.origin, in: size)
                    let rb = docPoint(
                        CGPoint(x: mr.maxX, y: mr.maxY), in: size)
                    let r = CGRect(x: min(ra.x, rb.x), y: min(ra.y, rb.y),
                                   width: abs(ra.x - rb.x), height: abs(ra.y - rb.y))
                    var hits = Set<UInt64>()
                    for l in doc.layers where l.kind != "adjustment" {
                        if let lb = doc.layerBounds[l.id], lb.intersects(r) {
                            hits.insert(l.id)
                        }
                    }
                    doc.selectedSet = hits
                    // primary selection = topmost hit in stack order
                    if let top = doc.layers.last(where: { hits.contains($0.id) }) {
                        doc.selected = top.id
                    }
                }
                // mask strokes dispatch paint-only: refresh state at
                // stroke end so hasMask/Invert/Clear chips surface
                if doc.maskArmed {
                    doc.reloadState()
                }
            }
    }

    private func commitTextEdit() {
        if let ed = doc.textEdit {
            var t = DocStore.find(ed.id, in: doc.layers)?.text ?? [:]
            t["text"] = ed.text
            doc.setLayer(ed.id, ["text": t], then: .reloadImage)
        }
        doc.textEdit = nil
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

    /// the fit rect transformed by the live view state (zoom × pan):
    /// everything that maps view↔doc space goes through this
    private func canvasRect(in size: CGSize) -> CGRect {
        let base = imageRect(in: size)
        let s = doc.zoom * pinchBy
        let w = base.width * s, h = base.height * s
        let cx = base.midX + doc.pan.width + pinchPan.width + dragPan.width
        let cy = base.midY + doc.pan.height + pinchPan.height + dragPan.height
        return CGRect(x: cx - w / 2, y: cy - h / 2, width: w, height: h)
    }

    /// view point → document pixel coords
    private func docPoint(_ p: CGPoint, in size: CGSize) -> CGPoint {
        let rect = canvasRect(in: size)
        guard doc.composite != nil, rect.width > 0, doc.docW > 0 else { return .zero }
        let sx = Double(doc.docW) / Double(rect.width)
        return CGPoint(x: Double(p.x - rect.minX) * sx,
                       y: Double(p.y - rect.minY) * sx)
    }

    private func brushViewSize(in size: CGSize) -> CGFloat {
        let rect = canvasRect(in: size)
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
                // group / ungroup — PS ⌘G equivalents
                Button { doc.groupSelection() } label: {
                    Image(systemName: "folder.badge.plus").font(.system(size: 10))
                }
                .buttonStyle(.plain).foregroundStyle(Kou.text2)
                .help("Group selected layers")
                Button {
                    if let s = doc.selected, doc.selLayer?.kind == "group" {
                        doc.ungroup(s)
                    }
                } label: {
                    Image(systemName: "folder.badge.minus").font(.system(size: 10))
                }
                .buttonStyle(.plain).foregroundStyle(
                    doc.selLayer?.kind == "group" ? Kou.text2 : Kou.text3.opacity(0.5))
                .help("Ungroup")
            }
            .padding(.horizontal, 10).padding(.vertical, 7)
            .overlay(alignment: .bottom) { Kou.hairline.frame(height: 1) }

            // vector shape generators — only meaningful on shape layers
            if doc.selLayer?.kind == "shape" {
                HStack(spacing: 5) {
                    ForEach([("rect", "square"), ("roundRect", "squareshape"),
                             ("ellipse", "circle"), ("star", "star"),
                             ("line", "line.diagonal")], id: \.0) { (gen, icon) in
                        Button { doc.addGenShape(gen) } label: {
                            Image(systemName: icon).font(.system(size: 10))
                                .frame(width: 22, height: 18)
                        }
                        .buttonStyle(.plain).foregroundStyle(Kou.accent)
                        .background(Kou.bg3.opacity(0.5)).cornerRadius(4)
                        .help("Add \(gen)")
                    }
                    Spacer()
                }
                .padding(.horizontal, 8).padding(.vertical, 5)
                .overlay(alignment: .bottom) { Kou.hairline.frame(height: 1) }
            }

            ScrollView {
                VStack(spacing: 2) {
                    layerRows(doc.layers, depth: 0)
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

    /// recursive rows: groups render children indented under a disclosure
    /// triangle; taps select, cmd-click adds to the marquee selection set;
    /// rows are draggable — drop between rows to reorder, onto a group
    /// row's middle band to reparent (Photoshop-style)
    @ViewBuilder
    private func layerRows(_ list: [DocLayer], depth: Int,
                           parent: UInt64? = nil) -> some View {
        ForEach(list.reversed()) { l in
            let sel = doc.selected == l.id || doc.selectedSet.contains(l.id)
            let edge = doc.dropEdge?.id == l.id
                         && doc.dropEdge?.parent == parent ? doc.dropEdge?.edge : nil
            LayerRow(doc: doc, layer: l, selected: sel, depth: depth,
                     inside: edge == .inside)
                .overlay(alignment: .top) {
                    if edge == .above {
                        Rectangle().fill(Kou.accent).frame(height: 2)
                            .padding(.horizontal, 2)
                    }
                }
                .overlay(alignment: .bottom) {
                    if edge == .below {
                        Rectangle().fill(Kou.accent).frame(height: 2)
                            .padding(.horizontal, 2)
                    }
                }
                .onTapGesture {
                    if NSEvent.modifierFlags.contains(.command) {
                        if doc.selectedSet.contains(l.id) {
                            doc.selectedSet.remove(l.id)
                        } else {
                            doc.selectedSet.insert(l.id)
                        }
                    }
                    doc.selected = l.id
                    if !doc.selectedSet.contains(l.id) && doc.selectedSet.isEmpty {
                        doc.selectedSet = [l.id]
                    }
                }
                .onDrag {
                    doc.draggingId = l.id
                    return NSItemProvider(object: NSString(string: "\(l.id)"))
                }
                .onDrop(of: [.text], delegate: LayerDropDelegate(
                    doc: doc, target: l, parentId: parent, siblings: list))
            if l.kind == "group", !doc.collapsed.contains(l.id), !l.children.isEmpty {
                // type-erased: recursive opaque types can't be inferred
                AnyView(layerRows(l.children, depth: depth + 1, parent: l.id))
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

enum DropEdge { case above, below, inside }

/// Photoshop-style layer-panel drop target: top/bottom edges reorder within
/// the target's parent; the middle band of a group row reparents into it.
struct LayerDropDelegate: DropDelegate {
    let doc: DocStore
    let target: DocLayer
    let parentId: UInt64?
    let siblings: [DocLayer]      // stack order, bottom→top

    private func edge(in info: DropInfo) -> DropEdge {
        let h: CGFloat = 26
        let y = info.location.y
        if target.kind == "group", y > h * 0.3, y < h * 0.7 { return .inside }
        return y < h / 2 ? .above : .below
    }

    /// is `needle` inside `hay`'s subtree (or hay itself)?
    private func containsLayer(_ hay: DocLayer, _ needle: UInt64) -> Bool {
        if hay.id == needle { return true }
        return hay.children.contains { containsLayer($0, needle) }
    }

    func validateDrop(info: DropInfo) -> Bool {
        guard let d = doc.draggingId, d != target.id else { return false }
        if let drag = DocStore.find(d, in: doc.layers),
           containsLayer(drag, target.id) { return false }
        return true
    }

    func dropEntered(info: DropInfo) {
        doc.dropEdge = (target.id, edge(in: info), parentId)
    }
    func dropUpdated(info: DropInfo) -> DropProposal? {
        doc.dropEdge = (target.id, edge(in: info), parentId)
        return DropProposal(operation: .move)
    }
    func dropExited(info: DropInfo) {
        if doc.dropEdge?.id == target.id { doc.dropEdge = nil }
    }
    func performDrop(info: DropInfo) -> Bool {
        defer { doc.dropEdge = nil; doc.draggingId = nil }
        guard let d = doc.draggingId, d != target.id else { return false }
        if edge(in: info) == .inside {
            doc.moveLayer(d, parent: target.id, to: target.children.count)
            return true
        }
        guard let ti = siblings.firstIndex(where: { $0.id == target.id })
        else { return false }
        var to = edge(in: info) == .above ? ti + 1 : ti
        // removal shifts the target index when the dragged layer sits below
        if let di = siblings.firstIndex(where: { $0.id == d }), di < ti { to -= 1 }
        doc.moveLayer(d, parent: parentId, to: max(to, 0))
        return true
    }
}

struct LayerRow: View {
    @ObservedObject var doc: DocStore
    let layer: DocLayer
    let selected: Bool
    var depth: Int = 0
    var inside: Bool = false

    var body: some View {
        HStack(spacing: 7) {
            if layer.kind == "group" {
                Image(systemName: doc.collapsed.contains(layer.id)
                      ? "chevron.right" : "chevron.down")
                    .font(.system(size: 7, weight: .bold))
                    .foregroundStyle(Kou.text3)
                    .frame(width: 10)
                    .onTapGesture {
                        if doc.collapsed.contains(layer.id) {
                            doc.collapsed.remove(layer.id)
                        } else {
                            doc.collapsed.insert(layer.id)
                        }
                    }
            } else if depth > 0 {
                Spacer().frame(width: 10)
            }
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
            if layer.kind == "group" {
                Text("\(layer.children.count)")
                    .font(.system(size: 8).monospacedDigit())
                    .foregroundStyle(Kou.text3)
            }
            Spacer()
            if layer.hasMask {
                Image(systemName: "rectangle.dashed")
                    .font(.system(size: 8)).foregroundStyle(Kou.text3)
            }
            if let st = layer.styles, !st.isEmpty {
                Text("fx")
                    .font(.system(size: 8, weight: .semibold, design: .rounded))
                    .foregroundStyle(Kou.accent)
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
        .background(selected || inside ? Kou.accentSoft : Color.clear)
        .clipShape(RoundedRectangle(cornerRadius: 5))
        .overlay(RoundedRectangle(cornerRadius: 5)
            .stroke(inside ? Kou.accent : (selected ? Kou.accent.opacity(0.5) : Color.clear),
                    lineWidth: inside ? 1.5 : 1))
    }
}

// MARK: - Inspector

struct LayerInspector: View {
    @ObservedObject var doc: DocStore
    let layer: DocLayer

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            commonPanel
            stylesPanel
            contentPanel
            maskPanel
        }
    }

    // ---- layer styles (drop shadow) ----

    // ---- layer styles (Photoshop-style effects) ----

    /// defaults per effect key (must match composer/src/style.rs serde defaults)
    private static let fxDefaults: [String: [String: Any]] = [
        "dropShadow": ["enabled": true, "blend": "multiply", "dx": 8.0, "dy": 8.0,
                       "blur": 12.0, "spread": 0.0, "color": [0.0, 0.0, 0.0, 0.5]],
        "innerShadow": ["enabled": true, "blend": "multiply", "dx": 4.0, "dy": 4.0,
                        "blur": 8.0, "choke": 0.0, "color": [0.0, 0.0, 0.0, 0.5]],
        "outerGlow": ["enabled": true, "blend": "screen", "blur": 16.0, "spread": 0.0,
                      "color": [1.0, 1.0, 0.75, 0.75]],
        "innerGlow": ["enabled": true, "blend": "screen", "source": "edge", "blur": 8.0,
                      "choke": 0.0, "color": [1.0, 1.0, 0.75, 0.75]],
        "bevel": ["enabled": true, "style": "innerBevel", "direction": "up", "size": 8.0,
                  "soften": 0.0, "angle": 120.0, "altitude": 30.0, "depth": 1.0,
                  "highlight": ["blend": "screen", "color": [1.0, 1.0, 1.0, 0.75]],
                  "shadow": ["blend": "multiply", "color": [0.0, 0.0, 0.0, 0.5]]],
        "satin": ["enabled": true, "blend": "multiply", "angle": 19.0, "distance": 11.0,
                  "size": 14.0, "invert": false, "color": [0.0, 0.0, 0.0, 0.4]],
        "colorOverlay": ["enabled": true, "blend": "normal", "color": [1.0, 1.0, 1.0, 1.0]],
        "gradientOverlay": ["enabled": true, "blend": "normal", "opacity": 1.0,
                            "gradient": ["angle": 90.0, "scale": 1.0, "style": "linear",
                                         "stops": [[0.0, 0.0, 0.0, 0.0, 1.0],
                                                   [1.0, 1.0, 1.0, 1.0, 1.0]]]],
        "patternOverlay": ["enabled": true, "blend": "normal", "opacity": 1.0, "scale": 1.0,
                           "pattern": ["kind": "builtin", "name": "checker", "size": 16.0,
                                       "fg": [0.0, 0.0, 0.0, 1.0], "bg": [1.0, 1.0, 1.0, 1.0]]],
        "stroke": ["enabled": true, "blend": "normal", "size": 3.0, "position": "outside",
                   "fill": ["fill": "color", "color": [0.0, 0.0, 0.0, 1.0]]],
    ]

    private func fx(_ key: String) -> [String: Any]? {
        layer.styles?[key] as? [String: Any]
    }

    private func fxOn(_ key: String) -> Bool { fx(key) != nil }

    private func setFx(_ key: String, _ changes: [String: Any]) {
        var st = layer.styles ?? [:]
        var e = fx(key) ?? (Self.fxDefaults[key] ?? [:])
        for (k, v) in changes { e[k] = v }
        st[key] = e
        doc.setLayerThrottled(layer.id, ["styles": st])
    }

    private func clearFx(_ key: String) {
        var st = layer.styles ?? [:]
        st.removeValue(forKey: key)
        doc.setLayer(layer.id,
                     ["styles": st.isEmpty ? NSNull() : st],
                     then: .reloadImage)
    }

    private func fxVal(_ key: String, _ p: String, _ def: Double) -> Double {
        (fx(key)?[p] as? NSNumber)?.doubleValue ?? def
    }

    private func fxNum(_ key: String, _ label: String, _ p: String, _ def: Double,
                       _ range: ClosedRange<Double>, reset: Double? = nil) -> some View {
        DebSliderRow(label, value: fxVal(key, p, def), range: range, reset: reset) {
            setFx(key, [p: $0])
        }
    }

    /// [r,g,b,a] channel of `effect.color`
    private func fxColorVal(_ key: String, _ i: Int, _ def: Double) -> Double {
        ((fx(key)?["color"] as? [Any])?
            .compactMap { ($0 as? NSNumber)?.doubleValue }[safe: i]) ?? def
    }

    private func setFxColor(_ key: String, _ i: Int, _ v: Double) {
        var c = (fx(key)?["color"] as? [Any])?
            .compactMap { ($0 as? NSNumber)?.doubleValue } ?? [0, 0, 0, 0.5]
        while c.count < 4 { c.append(0) }
        c[i] = v
        setFx(key, ["color": c])
    }

    @ViewBuilder
    private func fxColorRows(_ key: String, _ aDef: Double) -> some View {
        DebSliderRow("Color R", value: fxColorVal(key, 0, 0), range: 0...1) { setFxColor(key, 0, $0) }
        DebSliderRow("Color G", value: fxColorVal(key, 1, 0), range: 0...1) { setFxColor(key, 1, $0) }
        DebSliderRow("Color B", value: fxColorVal(key, 2, 0), range: 0...1) { setFxColor(key, 2, $0) }
        DebSliderRow("Opacity", value: fxColorVal(key, 3, aDef), range: 0...1, reset: aDef) { setFxColor(key, 3, $0) }
    }

    /// enum string param as a segmented picker
    private func fxEnum(_ key: String, _ p: String, _ options: [(String, String)]) -> some View {
        SegPicker(options.map { ($0.0, $0.1) }, selection: Binding(
            get: { (fx(key)?[p] as? String) ?? options.first?.0 ?? "" },
            set: { setFx(key, [p: $0]) }))
    }

    // gradient params live one level deeper: gradientOverlay.gradient
    private var gradSpec: [String: Any] {
        (fx("gradientOverlay")?["gradient"] as? [String: Any]) ?? [:]
    }

    private func gradVal(_ p: String, _ def: Double) -> Double {
        (gradSpec[p] as? NSNumber)?.doubleValue ?? def
    }

    private func setGrad(_ p: String, _ v: Any) {
        var g = (fx("gradientOverlay")?["gradient"] as? [String: Any])
            ?? ((Self.fxDefaults["gradientOverlay"]?["gradient"] as? [String: Any]) ?? [:])
        g[p] = v
        setFx("gradientOverlay", ["gradient": g])
    }

    // stroke fill lives one level deeper: stroke.fill.color
    private var strokeFill: [String: Any]? { fx("stroke")?["fill"] as? [String: Any] }

    private func strokeColorVal(_ i: Int) -> Double {
        ((strokeFill?["color"] as? [Any])?
            .compactMap { ($0 as? NSNumber)?.doubleValue }[safe: i]) ?? (i == 3 ? 1 : 0)
    }

    private func setStrokeColor(_ i: Int, _ v: Double) {
        var f = strokeFill ?? ["fill": "color", "color": [0.0, 0.0, 0.0, 1.0]]
        var c = (f["color"] as? [Any])?
            .compactMap { ($0 as? NSNumber)?.doubleValue } ?? [0, 0, 0, 1]
        while c.count < 4 { c.append(0) }
        c[i] = v
        f["color"] = c
        setFx("stroke", ["fill": f])
    }

    /// one collapsible effect section: header toggle + sliders when enabled
    private func fxSection<Content: View>(
        _ title: String, _ key: String,
        @ViewBuilder content: () -> Content
    ) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(title).font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(Kou.text2)
                Spacer()
                Toggle("", isOn: Binding(
                    get: { fxOn(key) },
                    set: { on in on ? setFx(key, [:]) : clearFx(key) }))
                    .labelsHidden().controlSize(.mini).tint(Kou.accent)
            }
            if fxOn(key) {
                content()
            }
        }
        .padding(.top, 2)
    }

    @ViewBuilder private var stylesPanel: some View {
        if layer.kind != "group" && layer.kind != "adjustment" {
            Panel("Layer Style") {
                fxSection("Drop Shadow", "dropShadow") {
                    fxNum("dropShadow", "Dist X", "dx", 8, -200...200, reset: 8)
                    fxNum("dropShadow", "Dist Y", "dy", 8, -200...200, reset: 8)
                    fxNum("dropShadow", "Blur", "blur", 12, 0...200, reset: 12)
                    fxNum("dropShadow", "Spread", "spread", 0, 0...1, reset: 0)
                    fxColorRows("dropShadow", 0.5)
                }
                fxSection("Inner Shadow", "innerShadow") {
                    fxNum("innerShadow", "Dist X", "dx", 4, -200...200, reset: 4)
                    fxNum("innerShadow", "Dist Y", "dy", 4, -200...200, reset: 4)
                    fxNum("innerShadow", "Blur", "blur", 8, 0...200, reset: 8)
                    fxNum("innerShadow", "Choke", "choke", 0, 0...1, reset: 0)
                    fxColorRows("innerShadow", 0.5)
                }
                fxSection("Outer Glow", "outerGlow") {
                    fxNum("outerGlow", "Blur", "blur", 16, 0...200, reset: 16)
                    fxNum("outerGlow", "Spread", "spread", 0, 0...1, reset: 0)
                    fxColorRows("outerGlow", 0.75)
                }
                fxSection("Inner Glow", "innerGlow") {
                    fxEnum("innerGlow", "source", [("edge", "Edge"), ("center", "Center")])
                    fxNum("innerGlow", "Blur", "blur", 8, 0...200, reset: 8)
                    fxNum("innerGlow", "Choke", "choke", 0, 0...1, reset: 0)
                    fxColorRows("innerGlow", 0.75)
                }
                fxSection("Bevel & Emboss", "bevel") {
                    fxEnum("bevel", "style", [("innerBevel", "Inner"), ("outerBevel", "Outer"),
                                              ("emboss", "Emboss"), ("pillowEmboss", "Pillow"),
                                              ("strokeEmboss", "Stroke")])
                    fxEnum("bevel", "direction", [("up", "Up"), ("down", "Down")])
                    fxNum("bevel", "Size", "size", 8, 0...64, reset: 8)
                    fxNum("bevel", "Soften", "soften", 0, 0...16, reset: 0)
                    fxNum("bevel", "Angle", "angle", 120, -180...180, reset: 120)
                    fxNum("bevel", "Altitude", "altitude", 30, 0...90, reset: 30)
                    fxNum("bevel", "Depth", "depth", 1, 0...4, reset: 1)
                }
                fxSection("Satin", "satin") {
                    fxNum("satin", "Angle", "angle", 19, -180...180, reset: 19)
                    fxNum("satin", "Distance", "distance", 11, 0...200, reset: 11)
                    fxNum("satin", "Size", "size", 14, 0...200, reset: 14)
                    fxColorRows("satin", 0.4)
                }
                fxSection("Color Overlay", "colorOverlay") {
                    fxColorRows("colorOverlay", 1.0)
                }
                fxSection("Gradient Overlay", "gradientOverlay") {
                    fxNum("gradientOverlay", "Opacity", "opacity", 1, 0...1, reset: 1)
                    DebSliderRow("Angle", value: gradVal("angle", 90),
                                 range: -180...180, reset: 90) { setGrad("angle", $0) }
                    DebSliderRow("Scale", value: gradVal("scale", 1),
                                 range: 0.1...4, reset: 1) { setGrad("scale", $0) }
                }
                fxSection("Pattern Overlay", "patternOverlay") {
                    fxNum("patternOverlay", "Opacity", "opacity", 1, 0...1, reset: 1)
                    fxNum("patternOverlay", "Scale", "scale", 1, 0.1...4, reset: 1)
                }
                fxSection("Stroke", "stroke") {
                    fxNum("stroke", "Size", "size", 3, 0...200, reset: 3)
                    fxEnum("stroke", "position", [("outside", "Out"), ("center", "Ctr"),
                                                  ("inside", "In")])
                    DebSliderRow("Color R", value: strokeColorVal(0), range: 0...1) { setStrokeColor(0, $0) }
                    DebSliderRow("Color G", value: strokeColorVal(1), range: 0...1) { setStrokeColor(1, $0) }
                    DebSliderRow("Color B", value: strokeColorVal(2), range: 0...1) { setStrokeColor(2, $0) }
                    DebSliderRow("Opacity", value: strokeColorVal(3), range: 0...1, reset: 1) { setStrokeColor(3, $0) }
                }
            }
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
                    ToolChip(label: "Invert", icon: "arrow.left.arrow.right") {
                        doc.dispatch(["id": "doc.maskInvert", "layer": layer.id])
                    }
                    ToolChip(label: "Clear", icon: "xmark") {
                        doc.setLayer(layer.id, ["mask": NSNull()])
                    }
                }
                Spacer()
            }
            if doc.maskArmed {
                SliderRow("Radius", $doc.brushRadius, 4...400, reset: 48)
                SliderRow("Softness", $doc.brushSoftness, 0...1, reset: 0.6)
                SegPicker([(true, "Hide"), (false, "Reveal")], selection: $doc.brushErase)
                Text("Drag on the canvas to paint the mask.")
                    .font(.system(size: 9)).foregroundStyle(Kou.text3)
            }
        }
    }
}

// MARK: - recipe inspector (develop / adjustment layers)

/// DaVinci-style tabbed inspector covering every engine recipe field —
/// reuses the same components the photo editor palettes use.
enum DocPalette: String, CaseIterable, Identifiable {
    case light, wheels, curves, zones, qualifier, windows, mixer, detail, fx
    var id: String { rawValue }
    var title: String {
        switch self {
        case .light: return "Light"
        case .wheels: return "Wheels"
        case .curves: return "Curves"
        case .zones: return "HDR Zones"
        case .qualifier: return "Qualifier"
        case .windows: return "Windows"
        case .mixer: return "Mixer"
        case .detail: return "Detail"
        case .fx: return "Effects"
        }
    }
    var icon: String {
        switch self {
        case .light: return "sun.max"
        case .wheels: return "circle.circle"
        case .curves: return "chart.line.uptrend.xyaxis"
        case .zones: return "square.split.bottomhalf.filled"
        case .qualifier: return "eyedropper.halffull"
        case .windows: return "circle.dashed"
        case .mixer: return "slider.horizontal.below.square.filled.and.square"
        case .detail: return "sparkle.magnifyingglass"
        case .fx: return "sparkles"
        }
    }
}

struct RecipeInspector: View {
    let layer: DocLayer
    @State var recipe: Recipe
    var onChange: (Recipe) -> Void
    @State private var debounce: Task<Void, Never>?
    @State private var pal: DocPalette = .light
    @State private var curveCh = 0

    init(layer: DocLayer, recipe: Recipe, onChange: @escaping (Recipe) -> Void) {
        self.layer = layer
        var r = recipe
        // pad fixed-size arrays so indexed controls never trap on a
        // malformed/short decoded recipe
        func pad(_ kp: WritableKeyPath<Recipe, [Double]>, _ n: Int,
                 _ fill: Double = 0) {
            while r[keyPath: kp].count < n { r[keyPath: kp].append(fill) }
        }
        pad(\.qh, 3); pad(\.qs, 3); pad(\.ql, 3); pad(\.qadj, 4)
        pad(\.q_clean, 2); pad(\.z_dark, 4); pad(\.z_shadow, 4)
        pad(\.z_light, 4); pad(\.z_global, 4); pad(\.mixer, 9)
        pad(\.zones_ev, 9); pad(\.mono, 3); pad(\.lift, 3)
        pad(\.gamma, 3, 1); pad(\.gain, 3, 1); pad(\.offset, 3)
        pad(\.flare, 4); pad(\.crop, 4); pad(\.wb_pick, 2, 0.5)
        self._recipe = State(initialValue: r)
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

    private func b3(_ kp: WritableKeyPath<Recipe, [Double]>) -> Binding<[Double]> {
        Binding(get: { recipe[keyPath: kp] },
                set: { recipe[keyPath: kp] = $0; push() })
    }

    private func bBool(_ kp: WritableKeyPath<Recipe, Bool>) -> Binding<Bool> {
        Binding(get: { recipe[keyPath: kp] },
                set: { recipe[keyPath: kp] = $0; push() })
    }

    /// safe indexed binding: pads short decoded arrays before writing so a
    /// malformed recipe can't crash the inspector
    private func bi(_ kp: WritableKeyPath<Recipe, [Double]>, _ i: Int,
                    _ def: Double = 0) -> Binding<Double> {
        Binding(
            get: { recipe[keyPath: kp][safe: i] ?? def },
            set: { nv in
                while recipe[keyPath: kp].count <= i {
                    recipe[keyPath: kp].append(def)
                }
                recipe[keyPath: kp][i] = nv
                push()
            })
    }

    private func bc(_ kp: WritableKeyPath<Recipe, [[Double]]>) -> Binding<[[Double]]> {
        Binding(get: { recipe[keyPath: kp] },
                set: { recipe[keyPath: kp] = $0; push() })
    }

    private func curveBinding(_ ch: Int) -> Binding<[[Double]]> {
        switch ch {
        case 0: return bc(\.curve)
        case 1: return bc(\.curve_r)
        case 2: return bc(\.curve_g)
        case 3: return bc(\.curve_b)
        case 4: return bc(\.hue_hue)
        case 5: return bc(\.hue_sat)
        case 6: return bc(\.hue_lum)
        case 7: return bc(\.lum_sat)
        default: return bc(\.sat_sat)
        }
    }

    private func curveName(_ ch: Int) -> String {
        ["Luma", "Red", "Green", "Blue", "Hue vs Hue", "Hue vs Sat",
         "Hue vs Lum", "Lum vs Sat", "Sat vs Sat"][ch]
    }

    private func curveTint(_ ch: Int) -> Color {
        [Color.white, .red, .green, .blue, Kou.accent, Kou.accent, Kou.accent,
         Kou.accent, Kou.accent][ch]
    }

    var body: some View {
        // palette strip — same icon chips as the photo editor
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 5) {
                ForEach(DocPalette.allCases) { p in
                    VStack(spacing: 2) {
                        Image(systemName: p.icon).font(.system(size: 11))
                        Text(p.title).font(.system(size: 7))
                    }
                    .frame(width: 40, height: 34)
                    .background(pal == p ? Kou.accentSoft : Color.clear)
                    .clipShape(RoundedRectangle(cornerRadius: 6))
                    .foregroundStyle(pal == p ? Kou.accent : Kou.text3)
                    .contentShape(Rectangle())
                    .onTapGesture { pal = p }
                }
            }
            .padding(.horizontal, 2)
        }

        Group {
            switch pal {
            case .light: lightPal
            case .wheels: wheelsPal
            case .curves: curvesPal
            case .zones: zonesPal
            case .qualifier: qualifierPal
            case .windows: windowsPal
            case .mixer: mixerPal
            case .detail: detailPal
            case .fx: fxPal
            }
        }
    }

    // ---- palettes ----

    private var lightPal: some View {
        VStack(alignment: .leading, spacing: 10) {
            if layer.kind == "adjustment" {
                Text("Crop/rotate/keystone are ignored on adjustment layers.")
                    .font(.system(size: 9)).foregroundStyle(Kou.text3)
            }
            Panel("White Balance") {
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
                SliderRow("Pivot", b(\.pivot), 0.05...0.5, reset: 0.18)
                SliderRow("Highlights", b(\.highlights), -1...1)
                SliderRow("Shadows", b(\.shadows), -1...1)
                SliderRow("Whites", b(\.whites), -1...1)
                SliderRow("Blacks", b(\.blacks), -1...1)
                SliderRow("Hi Rolloff", b(\.highlight_rolloff), 0...2, reset: 1)
                SliderRow("Sh Rolloff", b(\.shadow_rolloff), 0...2, reset: 1)
            }
            Panel("Color") {
                SliderRow("Saturation", b(\.saturation), -1...1)
                SliderRow("Vibrance", b(\.vibrance), -1...1)
            }
            Panel("Auto") {
                Toggle("Auto Exposure", isOn: bBool(\.auto_exposure))
                    .font(.system(size: 10.5)).foregroundStyle(Kou.text2)
                    .controlSize(.mini).tint(Kou.accent)
                Toggle("Auto Contrast", isOn: bBool(\.auto_contrast))
                    .font(.system(size: 10.5)).foregroundStyle(Kou.text2)
                    .controlSize(.mini).tint(Kou.accent)
            }
        }
    }

    private var wheelsPal: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("Color Wheels") {
                HStack(spacing: 8) {
                    ColorWheel(title: "Lift", v: b3(\.lift), center: 0)
                    ColorWheel(title: "Gamma", v: b3(\.gamma), center: 1)
                    ColorWheel(title: "Gain", v: b3(\.gain), center: 1)
                }
                .frame(height: 86)
                ColorWheel(title: "Offset", v: b3(\.offset), center: 0)
                    .frame(height: 86)
            }
            Panel("Split Tone") {
                SliderRow("Shd Hue", b(\.shadow_hue), 0...1, track: Kou.hueTrack)
                SliderRow("Shd Sat", b(\.shadow_sat), 0...1)
                SliderRow("Hi Hue", b(\.highlight_hue), 0...1, track: Kou.hueTrack)
                SliderRow("Hi Sat", b(\.highlight_sat), 0...1)
                SliderRow("Mid Hue", b(\.midtone_hue), 0...1, track: Kou.hueTrack)
                SliderRow("Mid Sat", b(\.midtone_sat), 0...1)
            }
            Panel("3D LUT") {
                HStack(spacing: 6) {
                    Text(recipe.lut_file.isEmpty ? "No LUT" :
                         URL(fileURLWithPath: recipe.lut_file).lastPathComponent)
                        .font(.system(size: 10)).foregroundStyle(Kou.text2)
                        .lineLimit(1).truncationMode(.middle)
                    Spacer()
                    ToolChip(label: "Pick .cube", icon: "folder") { pickLut() }
                    if !recipe.lut_file.isEmpty {
                        ToolChip(label: "Clear", icon: "xmark") {
                            recipe.lut_file = ""; push()
                        }
                    }
                }
                SliderRow("Amount", b(\.lut_amount), 0...1, reset: 1)
            }
        }
    }

    private func pickLut() {
        let p = NSOpenPanel()
        p.allowedContentTypes = [UTType(filenameExtension: "cube") ?? .data]
        if p.runModal() == .OK, let url = p.url {
            recipe.lut_file = url.path
            push()
        }
    }

    private var curvesPal: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("Curves") {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 4) {
                        ForEach(0..<9, id: \.self) { ch in
                            Text(curveName(ch))
                                .font(.system(size: 8,
                                              weight: curveCh == ch ? .bold : .regular))
                                .padding(.horizontal, 5).padding(.vertical, 2)
                                .background(curveCh == ch ? Kou.accentSoft : Color.clear)
                                .clipShape(Capsule())
                                .foregroundStyle(curveCh == ch ? Kou.accent : Kou.text3)
                                .onTapGesture { curveCh = ch }
                        }
                    }
                }
                CurveEditor(points: curveBinding(curveCh), tint: curveTint(curveCh),
                            spectrum: curveCh >= 4)
                    .frame(height: 150)
            }
        }
    }

    private var zonesPal: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("Tone Equalizer") {
                ZoneEQ(zones: b3(\.zones_ev))
            }
            Panel("HDR Zones") {
                ZoneRow("Dark", b3(\.z_dark))
                ZoneRow("Shadow", b3(\.z_shadow))
                ZoneRow("Light", b3(\.z_light))
                ZoneRow("Global", b3(\.z_global))
            }
        }
    }

    private var qualifierPal: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("Qualifier", trailing: {
                HStack(spacing: 6) {
                    Toggle("", isOn: bBool(\.q_enabled))
                        .labelsHidden().controlSize(.mini).tint(Kou.accent)
                    ToolChip(label: "Show", icon: "eye",
                             active: recipe.q_show) {
                        recipe.q_show.toggle(); push()
                    }
                }
            }) {
                RangeBar(title: "Hue", lo: bi(\.qh, 0, 0.5), hi: bi(\.qh, 1, 0.1),
                         gradient: Kou.hueTrack)
                SliderRow("H Soft", bi(\.qh, 2, 0.1), 0...0.5, reset: 0.1)
                RangeBar(title: "Sat", lo: bi(\.qs, 0), hi: bi(\.qs, 1, 1),
                         gradient: LinearGradient(colors: [.gray, .red],
                                                  startPoint: .leading, endPoint: .trailing))
                SliderRow("S Soft", bi(\.qs, 2, 0.1), 0...0.5, reset: 0.1)
                RangeBar(title: "Lum", lo: bi(\.ql, 0), hi: bi(\.ql, 1, 1),
                         gradient: LinearGradient(colors: [.black, .white],
                                                  startPoint: .leading, endPoint: .trailing))
                SliderRow("L Soft", bi(\.ql, 2, 0.1), 0...0.5, reset: 0.1)
                Toggle("Invert matte", isOn: bBool(\.q_invert))
                    .font(.system(size: 10.5)).foregroundStyle(Kou.text2)
                    .controlSize(.mini).tint(Kou.accent)
            }
            Panel("Adjust Selection") {
                SliderRow("Hue Shift", bi(\.qadj, 0), -0.5...0.5, track: Kou.hueTrack)
                SliderRow("Sat", bi(\.qadj, 1), -1...1)
                SliderRow("Lum", bi(\.qadj, 2), -1...1)
                SliderRow("Temp", bi(\.qadj, 3), -1...1, track: Kou.tempTrack)
            }
            Panel("Matte Finesse") {
                SliderRow("Clean Blk", bi(\.q_clean, 0), 0...1)
                SliderRow("Clean Wht", bi(\.q_clean, 1, 1), 0...1, reset: 1)
                SliderRow("Edge Blur", b(\.q_blur), 0...1)
            }
        }
    }

    private var windowsPal: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("Power Windows", trailing: {
                HStack(spacing: 6) {
                    ToolChip(label: "Circle", icon: "plus.circle") {
                        var w = PowerWindow()
                        w.kind = "circle"
                        recipe.windows.append(w); push()
                    }
                    ToolChip(label: "Grad", icon: "plus.rectangle") {
                        var w = PowerWindow()
                        w.kind = "gradient"
                        w.p = [0.3, 0.3, 0.7, 0.7, 0.4, 0]
                        recipe.windows.append(w); push()
                    }
                }
            }) {
                if recipe.windows.isEmpty {
                    Text("Add a circle or gradient window to isolate a region.")
                        .font(.system(size: 10)).foregroundStyle(Kou.text3)
                }
                ForEach(recipe.windows.indices, id: \.self) { i in
                    WindowRow(w: Binding(
                        get: { recipe.windows[safe: i] ?? PowerWindow() },
                        set: {
                            if i < recipe.windows.count { recipe.windows[i] = $0 }
                            push()
                        })) {
                        recipe.windows.remove(at: i); push()
                    }
                }
            }
        }
    }

    private var mixerPal: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("RGB Mixer") {
                VStack(spacing: 5) {
                    ForEach(0..<3, id: \.self) { row in
                        HStack(spacing: 5) {
                            Text(["R′", "G′", "B′"][row])
                                .font(.system(size: 10, weight: .semibold))
                                .foregroundStyle([Color.red.opacity(0.9),
                                                  .green.opacity(0.9),
                                                  .blue.opacity(0.9)][row])
                                .frame(width: 14, alignment: .leading)
                            MixRow(Binding(
                                get: { recipe.mixer },
                                set: { recipe.mixer = $0; push() }), row: row)
                        }
                    }
                }
                HStack {
                    Text("Monochrome").font(.system(size: 10.5)).foregroundStyle(Kou.text2)
                    Spacer()
                    Toggle("", isOn: Binding(
                        get: { recipe.mono != [0, 0, 0] },
                        set: { recipe.mono = $0 ? [0.21, 0.72, 0.07] : [0, 0, 0]; push() }
                    ))
                    .labelsHidden().controlSize(.mini).tint(Kou.accent)
                }
                if recipe.mono != [0, 0, 0] {
                    TriRow("Mono", b3(\.mono), 0...1)
                }
            }
        }
    }

    private var detailPal: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("Detail") {
                SliderRow("Sharpen", b(\.sharpen), 0...2)
                SliderRow("NR Luma", b(\.noise_luma), 0...1)
                SliderRow("NR Chroma", b(\.noise_chroma), 0...1)
                SliderRow("Deband", b(\.deband), 0...1)
                SliderRow("CA Fix", b(\.ca_fix), 0...1)
                SliderRow("Beauty", b(\.beauty), 0...1)
            }
        }
    }

    private var fxPal: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("Effects") {
                SliderRow("Clarity", b(\.clarity), -1...1)
                SliderRow("Vignette", b(\.vignette), -1...1)
                SliderRow("Grain", b(\.grain), 0...1)
                SliderRow("Glow", b(\.glow), 0...1)
                SliderRow("Flare", bi(\.flare, 2), 0...1)
                SliderRow("Fl Hue", bi(\.flare, 3), 0...1, track: Kou.hueTrack)
                SliderRow("Fl X", bi(\.flare, 0, 0.5), 0...1, reset: 0.5)
                SliderRow("Fl Y", bi(\.flare, 1, 0.5), 0...1, reset: 0.5)
            }
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
            HStack(spacing: 6) {
                Button("Apply path") {
                    setShapes([["d": dText, "fill": [0.96, 0.66, 0.24, 1]]])
                }
                .buttonStyle(KouSecondaryButton())
                ToolChip(label: doc.nodeEdit ? "Editing nodes" : "Edit nodes",
                         icon: "point.topleft.down.curvedto.point.bottomright.up",
                         active: doc.nodeEdit) {
                    doc.nodeEdit.toggle()
                    doc.refreshNodes()
                }
            }
            if doc.nodeEdit {
                Text("Drag the handles on the canvas — Esc exits node mode.")
                    .font(.system(size: 9)).foregroundStyle(Kou.text3)
            }
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
