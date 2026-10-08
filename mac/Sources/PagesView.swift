import SwiftUI
import AppKit
import UniformTypeIdentifiers

/// Pages workspace — a .kpages multi-page document driven by `pg.*` commands.
/// Left: page thumbnails (pg.renderPng), paired as verso/recto spreads when
/// facing is on; center: selected page preview + selectable frame outlines;
/// right: document/frame inspector.
final class PagesStore: ObservableObject {
    @Published var opened = false
    @Published var busy = false
    @Published var error: String?

    @Published var name = "Untitled"
    @Published var pageW = 595.0                     // A4 width in pt
    @Published var pageH = 842.0                     // A4 height in pt
    /// pt margins as {top,right,bottom,left} — koubou-pages Margins
    @Published var margins: [String: Double] = [
        "top": 40, "right": 40, "bottom": 40, "left": 40]
    private var marginL: Double { margins["left"] ?? 40 }
    private var marginR: Double { margins["right"] ?? 40 }
    private var marginT: Double { margins["top"] ?? 40 }
    @Published var facing = false                    // pg.setSpread
    @Published var spreadPairs: [[Int]] = []         // pg.json spread.pairs
    @Published var styles: [String] = []             // defined style names
    @Published var pages: [PgPage] = []
    @Published var selectedPage = 0
    @Published var selectedFrame: UInt64?
    @Published var previews: [Int: CGImage] = [:]
    @Published var dirty = false

    private var session: DocSession?

    struct PgPage: Identifiable {
        let id: Int
        var frames: [PgFrame]
    }
    struct PgFrame: Identifiable {
        let id: UInt64
        var kind: String
        var x: Double, y: Double, w: Double, h: Double
        var rotation: Double
        var text: String          // authored text
        var shown: String         // text after flow resolution (pg.json textFlow)
        var overset: Bool         // flow has unplaced remainder
        var style: String
        var next: UInt64?         // pg.linkFrames target
        var font: String
        var size: Double
        var leading: Double
        var color: [Double]
        var path: String
        var fill: [Double]
    }

    private func ensure() -> DocSession? {
        if session == nil { session = DocSession() }
        return session
    }

    enum After { case none, reload }

    func dispatch(_ cmd: [String: Any], then: After = .reload) {
        guard let s = ensure() else { error = "session failed"; return }
        busy = true
        Task.detached { [weak self] in
            let r = await s.work { $0.dispatch(cmd) }
            await MainActor.run {
                guard let self else { return }
                self.busy = false
                if r["ok"] as? Bool == true {
                    self.error = nil
                    self.dirty = true
                    if then == .reload { self.reloadState() }
                } else {
                    self.error = r["error"] as? String ?? "command failed"
                }
            }
        }
    }

    func newDoc() {
        opened = true
        dispatch(["id": "pg.new", "name": name, "w": pageW,
                  "h": pageH, "margins": margins, "facing": facing])
        // a fresh doc has zero pages — add one so the canvas isn't empty
        dispatch(["id": "pg.addPage"])
    }

    func open(_ url: URL) {
        opened = true
        dispatch(["id": "pg.open", "path": url.path])
    }

    func save() {
        let p = NSSavePanel()
        p.nameFieldStringValue = name + ".kpages"
        guard p.runModal() == .OK, let url = p.url else { return }
        dispatch(["id": "pg.save", "path": url.path], then: .none)
        dirty = false
    }

    func reloadState() {
        guard let s = session else { return }
        Task.detached { [weak self] in
            let dj = await s.work { $0.dispatch(["id": "pg.json"])["result"] as? [String: Any] ?? [:] }
            await MainActor.run {
                guard let self else { return }
                self.applyState(dj)
                self.renderAll()
            }
        }
    }

    private func applyState(_ d: [String: Any]) {
        name = d["name"] as? String ?? name
        pageW = (d["pageW"] as? NSNumber)?.doubleValue ?? pageW
        pageH = (d["pageH"] as? NSNumber)?.doubleValue ?? pageH
        facing = d["facing"] as? Bool ?? facing
        if let sp = d["spread"] as? [String: Any] {
            facing = sp["facing"] as? Bool ?? facing
            spreadPairs = (sp["pairs"] as? [[NSNumber]] ?? []).map { $0.map { $0.intValue } }
        } else {
            spreadPairs = []
        }
        styles = ((d["styles"] as? [String: Any]) ?? [:]).keys.sorted()
        // textFlow: frame id (string) → {text, overset}
        let flow = d["textFlow"] as? [String: Any] ?? [:]
        let pgs = d["pages"] as? [[String: Any]] ?? []
        pages = pgs.enumerated().map { (i, p) in
            let frs = (p["frames"] as? [[String: Any]] ?? []).map { f -> PgFrame in
                let fid = (f["id"] as? NSNumber)?.uint64Value ?? 0
                let fl = flow[String(fid)] as? [String: Any]
                return PgFrame(
                    id: fid,
                    kind: f["kind"] as? String ?? "rect",
                    x: (f["x"] as? NSNumber)?.doubleValue ?? 0,
                    y: (f["y"] as? NSNumber)?.doubleValue ?? 0,
                    w: (f["w"] as? NSNumber)?.doubleValue ?? 100,
                    h: (f["h"] as? NSNumber)?.doubleValue ?? 100,
                    rotation: (f["rotationDeg"] as? NSNumber)?.doubleValue ?? 0,
                    text: f["text"] as? String ?? "",
                    shown: fl?["text"] as? String ?? f["text"] as? String ?? "",
                    overset: fl?["overset"] as? Bool ?? false,
                    style: f["style"] as? String ?? "",
                    next: (f["next"] as? NSNumber)?.uint64Value,
                    font: f["font"] as? String ?? "",
                    size: (f["size"] as? NSNumber)?.doubleValue ?? 12,
                    leading: (f["leading"] as? NSNumber)?.doubleValue ?? 1.2,
                    color: (f["color"] as? [NSNumber] ?? []).map { $0.doubleValue },
                    path: f["path"] as? String ?? "",
                    fill: (f["fill"] as? [NSNumber] ?? []).map { $0.doubleValue })
            }
            return PgPage(id: i, frames: frs)
        }
        if selectedPage >= pages.count { selectedPage = max(0, pages.count - 1) }
    }

    /// pg.renderPng writes to a path — render into /tmp and load it back.
    func renderPage(_ i: Int) {
        guard let s = session, pages.indices.contains(i) else { return }
        let out = NSTemporaryDirectory() + "koubou_pgp_\(i).png"
        Task.detached { [weak self] in
            let img = await s.work { sess -> CGImage? in
                let r = sess.dispatch(["id": "pg.renderPng",
                                       "page": i, "out": out, "dpi": 96.0])
                guard r["ok"] as? Bool == true,
                      let ns = NSImage(contentsOfFile: out) else { return nil }
                var rect = CGRect(origin: .zero, size: ns.size)
                return ns.cgImage(forProposedRect: &rect, context: nil, hints: nil)
            }
            await MainActor.run { self?.previews[i] = img }
        }
    }

    /// re-render every thumbnail (page size / facing / reflow changes)
    func renderAll() {
        for pg in pages { renderPage(pg.id) }
    }

    // ---- editing ops ----

    func addPage() {
        dispatch(["id": "pg.addPage"])
    }
    func removePage() {
        dispatch(["id": "pg.removePage", "page": selectedPage])
    }
    func setPageSize(_ w: Double, _ h: Double) {
        dispatch(["id": "pg.setPageSize", "w": w, "h": h])
    }
    func setFacing(_ on: Bool) {
        dispatch(["id": "pg.setSpread", "facing": on])
    }
    func linkFrames(_ from: UInt64, to: UInt64?) {
        var cmd: [String: Any] = ["id": "pg.linkFrames", "frame": from]
        cmd["to"] = to.map { NSNumber(value: $0) } ?? NSNull()
        dispatch(cmd)
    }
    func applyStyle(_ frame: UInt64, name: String) {
        dispatch(["id": "pg.applyStyle", "frame": frame, "name": name])
    }
    /// capture the frame's current text props as a named style
    func defineStyle(_ name: String, from f: PgFrame) {
        var cmd: [String: Any] = ["id": "pg.setStyle", "name": name,
                                  "size": f.size, "leading": f.leading,
                                  "color": f.color.isEmpty ? [0, 0, 0, 1] : f.color]
        if !f.font.isEmpty { cmd["font"] = f.font }
        dispatch(cmd)
    }
    func addFrame(kind: String) {
        var cmd: [String: Any] = ["id": "pg.addFrame", "page": selectedPage,
                                  "kind": kind,
                                  "x": marginL, "y": marginT,
                                  "w": pageW - marginL - marginR,
                                  "h": 160.0]
        switch kind {
        case "text":
            cmd["text"] = "Heading"; cmd["size"] = 24.0; cmd["wrap"] = true
        case "image":
            let p = NSOpenPanel()
            p.allowedContentTypes = [.png, .jpeg]
            guard p.runModal() == .OK, let url = p.url else { return }
            cmd["path"] = url.path; cmd["fit"] = "fit"
        case "rect":
            cmd["fill"] = [0.9, 0.85, 0.75, 1]
        case "line":
            cmd["h"] = 40.0
            cmd["x2"] = cmd["w"]!; cmd["y2"] = 0.0
            cmd["stroke"] = ["color": [0.2, 0.2, 0.2, 1], "width": 1.5]
        default: break
        }
        dispatch(cmd)
    }
    func setFrame(_ id: UInt64, _ params: [String: Any]) {
        var cmd: [String: Any] = ["id": "pg.setFrame", "frame": id]
        cmd.merge(params) { _, n in n }
        dispatch(cmd)
    }
    func removeFrame() {
        guard let id = selectedFrame else { return }
        dispatch(["id": "pg.removeFrame", "frame": id])
        selectedFrame = nil
    }

    func exportPdf() {
        let p = NSSavePanel()
        p.nameFieldStringValue = name + ".pdf"
        guard p.runModal() == .OK, let url = p.url else { return }
        dispatch(["id": "pg.render", "out": url.path], then: .none)
    }
}

struct PagesView: View {
    @ObservedObject var p: PagesStore
    @State private var styleName = ""

    /// (name, w×h pt) paper presets — must match the engine's pt units
    private static let papers: [(String, Double, Double)] = [
        ("A4", 595, 842), ("Letter", 612, 792),
        ("A5", 420, 595), ("B5", 516, 729),
    ]

    private var paperSelection: Binding<String> {
        Binding(
            get: {
                Self.papers.first { abs($0.1 - p.pageW) < 0.5 && abs($0.2 - p.pageH) < 0.5 }?.0
                    ?? "Custom"
            },
            set: { v in
                guard let (_, w, h) = Self.papers.first(where: { $0.0 == v }) else { return }
                p.setPageSize(w, h)
            })
    }

    var body: some View {
        HSplitView {
            pageList.frame(minWidth: 160, idealWidth: 190, maxWidth: 240)
            canvas
            inspector.frame(minWidth: 240, idealWidth: 280, maxWidth: 340)
        }
    }

    // ---- page thumbnails — paired when facing pages is on ----

    private var pageList: some View {
        VStack(spacing: 0) {
            HStack {
                ToolChip(label: "Page", icon: "plus") { p.addPage() }
                Spacer()
                if p.busy { ProgressView().controlSize(.mini) }
            }
            .padding(8)
            Divider().overlay(Kou.hairline)
            ScrollView {
                VStack(spacing: 10) {
                    if p.facing && !p.spreadPairs.isEmpty {
                        ForEach(Array(p.spreadPairs.enumerated()), id: \.offset) { _, pair in
                            spreadRow(pair)
                        }
                    } else {
                        ForEach(p.pages) { page in
                            pageThumb(page)
                        }
                    }
                }
                .padding(10)
            }
        }
        .background(Kou.bg1)
    }

    /// one spread row: cover alone, then [verso, recto] pairs side by side
    private func spreadRow(_ pair: [Int]) -> some View {
        HStack(spacing: 4) {
            ForEach(pair, id: \.self) { idx in
                if p.pages.indices.contains(idx) {
                    pageThumb(p.pages[idx])
                }
            }
        }
    }

    private func pageThumb(_ page: PagesStore.PgPage) -> some View {
        let ar = p.pageH / max(p.pageW, 1)
        return ZStack(alignment: .bottomLeading) {
            ZStack {
                RoundedRectangle(cornerRadius: 3).fill(Color.white)
                if let img = p.previews[page.id] {
                    Image(decorative: img, scale: 1).resizable().scaledToFit()
                }
                if p.selectedPage == page.id {
                    RoundedRectangle(cornerRadius: 3).stroke(Kou.accent, lineWidth: 2)
                }
            }
            Text("\(page.id + 1)")
                .font(.system(size: 8, weight: .medium))
                .foregroundStyle(Kou.text3)
                .padding(.horizontal, 4).padding(.vertical, 2)
                .background(Capsule().fill(Kou.bg3.opacity(0.85)))
                .padding(3)
        }
        .aspectRatio(1 / ar, contentMode: .fit)
        .onTapGesture { p.selectedPage = page.id; p.selectedFrame = nil; p.renderPage(page.id) }
    }

    private var canvas: some View {
        GeometryReader { geo in
            let scale = min(geo.size.width * 0.9 / max(p.pageW, 1),
                            geo.size.height * 0.9 / max(p.pageH, 1))
            VStack {
                ZStack(alignment: .topLeading) {
                    Rectangle().fill(Color.white)
                    if let img = p.previews[p.selectedPage] {
                        Image(decorative: img, scale: 1).resizable().scaledToFit()
                            .frame(width: p.pageW * scale, height: p.pageH * scale)
                    }
                    if p.pages.indices.contains(p.selectedPage) {
                        ForEach(p.pages[p.selectedPage].frames) { f in
                            frameOutline(f, scale: scale)
                        }
                    }
                }
                .frame(width: p.pageW * scale, height: p.pageH * scale)
                .shadow(radius: 8)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .background(Kou.bg0)
    }

    private func frameOutline(_ f: PagesStore.PgFrame, scale: Double) -> some View {
        ZStack(alignment: .topTrailing) {
            RoundedRectangle(cornerRadius: 2)
                .stroke(p.selectedFrame == f.id ? Kou.accent : Color.blue.opacity(0.45),
                        lineWidth: p.selectedFrame == f.id ? 2 : 1)
            if f.overset {
                // the overset badge mirrors InDesign's red [+]
                Text("+")
                    .font(.system(size: 9, weight: .bold))
                    .foregroundStyle(.white)
                    .padding(.horizontal, 4).padding(.vertical, 1)
                    .background(RoundedRectangle(cornerRadius: 2).fill(Color.red))
                    .offset(x: 4, y: f.h * scale - 8)
            }
            if let next = f.next {
                Text("→\(next)")
                    .font(.system(size: 8, weight: .medium))
                    .foregroundStyle(Kou.accent)
                    .padding(.horizontal, 3).padding(.vertical, 1)
                    .background(Capsule().fill(Kou.accentSoft))
                    .padding(2)
            }
        }
        .frame(width: f.w * scale, height: f.h * scale)
        .contentShape(Rectangle())
        .offset(x: f.x * scale, y: f.y * scale)
        .onTapGesture { p.selectedFrame = f.id }
    }

    // ---- inspector ----

    private var inspector: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 10) {
                HStack {
                    Text("Pages").font(.headline)
                    Spacer()
                    Button("Export PDF…") { p.exportPdf() }.buttonStyle(.borderedProminent)
                        .controlSize(.small)
                }

                Panel("Document") {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("PAPER").font(.system(size: 8.5, weight: .semibold))
                            .tracking(0.8).foregroundStyle(Kou.text3)
                        SegPicker(Self.papers.map { ($0.0, $0.0) } + [("Custom", "Custom")],
                                  selection: paperSelection)
                        HStack {
                            Text("Facing pages").font(.system(size: 11))
                                .foregroundStyle(Kou.text1)
                            Spacer()
                            ToolChip(label: p.facing ? "On" : "Off",
                                     icon: "book.pages",
                                     active: p.facing) { p.setFacing(!p.facing) }
                        }
                    }
                }

                Panel("Add Frame") {
                    HStack(spacing: 6) {
                        ForEach(["text", "image", "rect", "line"], id: \.self) { k in
                            ToolChip(label: k.capitalized, icon: "plus") { p.addFrame(kind: k) }
                        }
                    }
                }

                if let id = p.selectedFrame,
                   p.pages.indices.contains(p.selectedPage),
                   let f = p.pages[p.selectedPage].frames.first(where: { $0.id == id }) {
                    frameInspector(f)
                } else {
                    Panel("Frame") {
                        Text("Select a frame on the canvas")
                            .font(.caption).foregroundStyle(Kou.text3)
                    }
                }

                if let e = p.error {
                    Text(e).foregroundStyle(.red).font(.caption)
                }
            }
            .padding(12)
        }
        .background(Kou.bg1)
    }

    private func frameInspector(_ f: PagesStore.PgFrame) -> some View {
        Panel("Frame \(f.id) · \(f.kind)") {
            VStack(alignment: .leading, spacing: 6) {
                frow("x", f.x) { p.setFrame(f.id, ["x": $0]) }
                frow("y", f.y) { p.setFrame(f.id, ["y": $0]) }
                frow("w", f.w) { p.setFrame(f.id, ["w": $0]) }
                frow("h", f.h) { p.setFrame(f.id, ["h": $0]) }
                frow("rot°", f.rotation) { p.setFrame(f.id, ["rotationDeg": $0]) }

                if f.kind == "text" {
                    Divider().overlay(Kou.hairline)
                    // the text the frame actually shows after link resolution
                    Text(f.shown.isEmpty ? "(empty)" : f.shown)
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(f.shown.isEmpty ? Kou.text3 : Kou.text2)
                        .lineLimit(5)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    if f.overset {
                        Text("overset — link to another frame")
                            .font(.system(size: 9, weight: .medium))
                            .foregroundStyle(.red)
                    }
                    // text threading: enter a target frame id, or unlink
                    HStack(spacing: 6) {
                        Text("Flow to").font(.caption).foregroundStyle(Kou.text2)
                        FlowTargetField(frame: f) { target in
                            p.linkFrames(f.id, to: target)
                        }
                        if f.next != nil {
                            ToolChip(label: "Unlink", icon: "xmark") {
                                p.linkFrames(f.id, to: nil)
                            }
                        }
                    }
                    // paragraph style: apply a named style, or define one
                    // from this frame's current props
                    HStack(spacing: 6) {
                        Text("Style").font(.caption).foregroundStyle(Kou.text2)
                        TextField("name", text: $styleName)
                            .textFieldStyle(.roundedBorder).font(.caption)
                        ToolChip(label: "Apply", icon: "textformat") {
                            if !styleName.isEmpty { p.applyStyle(f.id, name: styleName) }
                        }
                        .disabled(styleName.isEmpty)
                        ToolChip(label: "Define", icon: "tray.and.arrow.down") {
                            if !styleName.isEmpty { p.defineStyle(styleName, from: f) }
                        }
                        .disabled(styleName.isEmpty)
                    }
                    if !f.style.isEmpty {
                        Text("applied: \(f.style)")
                            .font(.system(size: 9)).foregroundStyle(Kou.text3)
                    }
                    if !p.styles.isEmpty {
                        ScrollView(.horizontal, showsIndicators: false) {
                            HStack(spacing: 4) {
                                ForEach(p.styles, id: \.self) { s in
                                    ToolChip(label: s) { p.applyStyle(f.id, name: s) }
                                }
                            }
                        }
                    }
                }

                Divider().overlay(Kou.hairline)
                HStack {
                    Spacer()
                    ToolChip(label: "Delete frame", icon: "trash") { p.removeFrame() }
                }
            }
        }
    }

    private func frow(_ label: String, _ val: Double, commit: @escaping (Double) -> Void) -> some View {
        PgRow(label: label, val: val, commit: commit)
    }
}

/// target frame id field for pg.linkFrames — empty means unlinked
private struct FlowTargetField: View {
    let frame: PagesStore.PgFrame
    let commit: (UInt64?) -> Void
    @State private var text = ""
    var body: some View {
        TextField("—", text: $text)
            .textFieldStyle(.roundedBorder).font(.caption).frame(width: 44)
            .onAppear { text = frame.next.map { "\($0)" } ?? "" }
            .onChange(of: frame.next) { _, v in text = v.map { "\($0)" } ?? "" }
            .onSubmit {
                if text.trimmingCharacters(in: .whitespaces).isEmpty {
                    commit(nil)
                } else if let t = UInt64(text) {
                    commit(t)
                }
            }
    }
}

private struct PgRow: View {
    let label: String
    let val: Double
    let commit: (Double) -> Void
    @State private var text = ""
    var body: some View {
        HStack {
            Text(label).font(.caption).foregroundStyle(Kou.text2)
                .frame(width: 40, alignment: .leading)
            TextField("", text: $text)
                .textFieldStyle(.roundedBorder).font(.caption).frame(width: 70)
                .onAppear { text = String(format: "%.1f", val) }
                .onChange(of: val) { _, v in text = String(format: "%.1f", v) }
                .onSubmit { if let v = Double(text) { commit(v) } }
        }
    }
}
