import SwiftUI
import AppKit
import UniformTypeIdentifiers

/// Pages workspace — a .kpages multi-page document driven by `pg.*` commands.
/// Left: page thumbnails (pg.renderPng); center: selected page preview +
/// selectable frame outlines; right: frame/page inspector.
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
        var text: String
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
                  "h": pageH, "margins": margins])
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
                self.renderPage(self.selectedPage)
            }
        }
    }

    private func applyState(_ d: [String: Any]) {
        name = d["name"] as? String ?? name
        pageW = (d["pageW"] as? NSNumber)?.doubleValue ?? pageW
        pageH = (d["pageH"] as? NSNumber)?.doubleValue ?? pageH
        let pgs = d["pages"] as? [[String: Any]] ?? []
        pages = pgs.enumerated().map { (i, p) in
            let frs = (p["frames"] as? [[String: Any]] ?? []).map { f -> PgFrame in
                PgFrame(id: (f["id"] as? NSNumber)?.uint64Value ?? 0,
                        kind: f["kind"] as? String ?? "rect",
                        x: (f["x"] as? NSNumber)?.doubleValue ?? 0,
                        y: (f["y"] as? NSNumber)?.doubleValue ?? 0,
                        w: (f["w"] as? NSNumber)?.doubleValue ?? 100,
                        h: (f["h"] as? NSNumber)?.doubleValue ?? 100,
                        rotation: (f["rotationDeg"] as? NSNumber)?.doubleValue ?? 0,
                        text: f["text"] as? String ?? "",
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

    // ---- editing ops ----

    func addPage() {
        dispatch(["id": "pg.addPage"])
    }
    func removePage() {
        dispatch(["id": "pg.removePage", "page": selectedPage])
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
    @State private var paperSize = "A4"

    var body: some View {
        HSplitView {
            pageList.frame(minWidth: 150, idealWidth: 180, maxWidth: 220)
            canvas
            inspector.frame(minWidth: 230, idealWidth: 270, maxWidth: 330)
        }
    }

    private var pageList: some View {
        VStack(spacing: 0) {
            HStack {
                Button("+ Page") { p.addPage() }.buttonStyle(.borderless)
                Spacer()
                if p.busy { ProgressView().controlSize(.mini) }
            }
            .padding(8)
            Divider()
            ScrollView {
                VStack(spacing: 10) {
                    ForEach(p.pages) { page in
                        pageThumb(page)
                    }
                }
                .padding(10)
            }
        }
        .background(Kou.bg1)
    }

    private func pageThumb(_ page: PagesStore.PgPage) -> some View {
        let ar = p.pageH / max(p.pageW, 1)
        return ZStack {
            RoundedRectangle(cornerRadius: 3).fill(Color.white)
            if let img = p.previews[page.id] {
                Image(decorative: img, scale: 1).resizable().scaledToFit()
            }
            if p.selectedPage == page.id {
                RoundedRectangle(cornerRadius: 3).stroke(Kou.accent, lineWidth: 2)
            }
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
                            RoundedRectangle(cornerRadius: 2)
                                .stroke(p.selectedFrame == f.id ? Kou.accent : Color.blue.opacity(0.45),
                                        lineWidth: p.selectedFrame == f.id ? 2 : 1)
                                .frame(width: f.w * scale, height: f.h * scale)
                                .contentShape(Rectangle())
                                .offset(x: f.x * scale, y: f.y * scale)
                                .onTapGesture { p.selectedFrame = f.id }
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

    private var inspector: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("Pages").font(.headline)
                Spacer()
                Button("Export PDF…") { p.exportPdf() }.buttonStyle(.borderedProminent)
            }
            Picker("Paper", selection: $paperSize) {
                Text("A4").tag("A4"); Text("Letter").tag("Letter")
            }
            .pickerStyle(.segmented)
            .onChange(of: paperSize) { _, v in
                let (w, h) = v == "A4" ? (595.0, 842.0) : (612.0, 792.0)
                p.dispatch(["id": "pg.setFrame", "page": 0, "frame": 0]) // no-op keep compile
                p.pageW = w; p.pageH = h
            }
            Divider()
            HStack(spacing: 6) {
                ForEach(["text", "image", "rect", "line"], id: \.self) { k in
                    Button("+\(k)") { p.addFrame(kind: k) }.buttonStyle(.bordered)
                }
            }
            if let id = p.selectedFrame,
               p.pages.indices.contains(p.selectedPage),
               let f = p.pages[p.selectedPage].frames.first(where: { $0.id == id }) {
                Text("Frame \(id) · \(f.kind)").font(.subheadline).bold()
                frow("x", f.x) { p.setFrame(id, ["x": $0]) }
                frow("y", f.y) { p.setFrame(id, ["y": $0]) }
                frow("w", f.w) { p.setFrame(id, ["w": $0]) }
                frow("h", f.h) { p.setFrame(id, ["h": $0]) }
                frow("rot°", f.rotation) { p.setFrame(id, ["rotationDeg": $0]) }
                if f.kind == "text" {
                    TextField("Text", text: .constant(f.text), axis: .vertical)
                        .textFieldStyle(.roundedBorder).font(.caption).lineLimit(4)
                        .disabled(true)
                        .help("edit via pg.setFrame text — inline editor pending")
                }
                Button("Delete Frame", role: .destructive) { p.removeFrame() }
            } else {
                Text("Select a frame").foregroundStyle(.secondary)
            }
            Spacer()
            if let e = p.error { Text(e).foregroundStyle(.red).font(.caption) }
        }
        .padding(12)
        .background(Kou.bg1)
    }

    private func frow(_ label: String, _ val: Double, commit: @escaping (Double) -> Void) -> some View {
        PgRow(label: label, val: val, commit: commit)
    }
}

private struct PgRow: View {
    let label: String
    let val: Double
    let commit: (Double) -> Void
    @State private var text = ""
    var body: some View {
        HStack {
            Text(label).font(.caption).frame(width: 40, alignment: .leading)
            TextField("", text: $text)
                .textFieldStyle(.roundedBorder).font(.caption).frame(width: 70)
                .onAppear { text = String(format: "%.1f", val) }
                .onChange(of: val) { _, v in text = String(format: "%.1f", v) }
                .onSubmit { if let v = Double(text) { commit(v) } }
        }
    }
}
