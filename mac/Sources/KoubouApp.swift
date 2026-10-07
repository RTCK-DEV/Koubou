import SwiftUI
import AppKit
import UniformTypeIdentifiers

@main
struct KouApp: App {
    @StateObject private var store = LibraryStore()

    var body: some Scene {
        WindowGroup("koubou") {
            StudioView()
                .environmentObject(store)
                .frame(minWidth: 1120, minHeight: 700)
        }
        .windowToolbarStyle(.unifiedCompact)
        .commands {
            CommandGroup(after: .newItem) {
                Button("Open Folder…") { store.pickFolder() }
                    .keyboardShortcut("o", modifiers: .command)
                Divider()
                Button("New Document") { store.newDocument() }
                    .keyboardShortcut("n", modifiers: .command)
                Button("Open Document…") { store.openDocument() }
                    .keyboardShortcut("o", modifiers: [.command, .shift])
                Button("Import PDF as Document…") { store.doc.importPdf() }
                Divider()
                Button("New Timeline") { store.mode = .motion }
                Button("New Pages Doc") { store.mode = .pages }
            }
        }
    }
}

/// Workspace modes — one window, every craft domain.
enum StudioMode: String, CaseIterable {
    case library = "Library"
    case doc = "Layers"
    case motion = "Motion"
    case pages = "Pages"
}

struct StudioView: View {
    @EnvironmentObject var store: LibraryStore

    var body: some View {
        VStack(spacing: 0) {
            modeBar
            Divider()
            content
        }
        .background(Kou.bg0)
    }

    private var modeBar: some View {
        HStack(spacing: 4) {
            ForEach(StudioMode.allCases, id: \.self) { m in
                Button {
                    store.mode = m
                } label: {
                    Text(m.rawValue)
                        .font(.system(size: 11.5, weight: .medium))
                        .padding(.horizontal, 12).padding(.vertical, 5)
                        .background(
                            Capsule().fill(store.mode == m ? Kou.accent.opacity(0.9) : Color.clear))
                        .foregroundStyle(store.mode == m ? .black : .secondary)
                }
                .buttonStyle(.plain)
            }
            Spacer()
        }
        .padding(.horizontal, 10).padding(.vertical, 7)
    }

    @ViewBuilder private var content: some View {
        switch store.mode {
        case .library: LibraryView()
        case .doc: DocEditorView(doc: store.doc) { store.mode = .library }
        case .motion: MotionView(m: store.motion)
        case .pages: PagesView(p: store.pages)
        }
    }
}

final class LibraryStore: ObservableObject {
    @Published var photos: [Photo] = []
    @Published var folder: URL?
    @Published var selection: Photo?
    /// layered-document workspace (koubou-composer session)
    let doc = DocStore()
    /// video timeline workspace (koubou-motion session)
    let motion = MotionStore()
    /// page-layout workspace (koubou-pages session)
    let pages = PagesStore()
    /// active workspace
    @Published var mode: StudioMode = .library {
        didSet {
            // entering a workspace lazily creates its document
            if mode == .motion && !motion.opened { motion.newTimeline() }
            if mode == .pages && !pages.opened { pages.newDoc() }
        }
    }
    @Published var editingDoc = false {
        didSet { if editingDoc { mode = .doc } }
    }
    @Published var scanning = false
    @Published var minRating = 0
    @Published var labelFilter = ""
    @Published var cameraFilter = ""
    @Published var lensFilter = ""
    /// darktable lighttable sort key: name | date | rating | size
    @Published var sortKey = "name"

    /// Recipes edited but not yet saved, keyed by photo path — keeps unsaved
    /// work alive when switching photos. Cleared when the folder reloads.
    var unsavedEdits: [String: Recipe] = [:]

    /// Same idea for grade-version lists (they live in the sidecar file).
    var unsavedVersions: [String: [GradeVersion]] = [:]

    /// distinct camera/lens names seen in the current folder (filter menus)
    var cameras: [String] { Array(Set(photos.map(\.camera).filter { !$0.isEmpty })).sorted() }
    var lenses: [String] { Array(Set(photos.map(\.lens).filter { !$0.isEmpty })).sorted() }

    var filtered: [Photo] {
        let f = photos.filter {
            $0.rating >= minRating
                && (labelFilter.isEmpty || $0.label == labelFilter)
                && (cameraFilter.isEmpty || $0.camera == cameraFilter)
                && (lensFilter.isEmpty || $0.lens == lensFilter)
        }
        switch sortKey {
        case "date":   return f.sorted { $0.mtime < $1.mtime }
        case "rating": return f.sorted { $0.rating > $1.rating }
        case "size":   return f.sorted { $0.size > $1.size }
        default:       return f.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
        }
    }

    /// Single rating write path for the whole app — thumbnail hover-stars,
    /// the editor's star row and digit keys all go through here so the
    /// sidecar, the grid and the open editor can never disagree.
    /// `r` is absolute (0-5); callers wanting toggle semantics compute it.
    func setRating(path: String, _ r: Int) {
        let nr = min(max(r, 0), 5)
        _ = eng.setRating(path: path, nr)
        if let i = photos.firstIndex(where: { $0.path == path }) {
            photos[i].rating = nr
        }
    }

    let eng = KouEngine.shared

    func pickFolder() {
        let p = NSOpenPanel()
        p.canChooseDirectories = true
        p.canChooseFiles = false
        p.allowsMultipleSelection = false
        if p.runModal() == .OK, let url = p.url {
            open(url)
        }
    }

    func open(_ url: URL) {
        let switching = folder != url
        folder = url
        if switching {
            // a different folder: drafts and selection belong to the old list
            unsavedEdits.removeAll()
            unsavedVersions.removeAll()
            selection = nil
        }
        // a rescan of the same folder keeps unsaved edits + selection alive
        scanning = true
        Task.detached { [weak self] in
            let photos = await KouEngine.shared.work { $0.scan(folder: url.path) }
            await MainActor.run {
                self?.photos = photos
                self?.scanning = false
                if let sel = self?.selection,
                   photos.contains(where: { $0.path == sel.path }) {
                    // keep the selection on rescan (fresh object for equality)
                    self?.selection = photos.first { $0.path == sel.path }
                } else {
                    self?.selection = photos.first
                }
            }
        }
    }

    func refresh() {
        guard let url = folder else { return }
        open(url)
    }

    // ---- layered document workspace ----

    /// Promote the selected photo into a layered document and switch views.
    func editAsDocument() {
        guard let p = selection else { return }
        mode = .doc
        doc.fromPhoto(p.path)
    }

    func openDocument() {
        let p = NSOpenPanel()
        p.canChooseFiles = true
        p.canChooseDirectories = false
        if let t = UTType(filenameExtension: "koubou") {
            p.allowedContentTypes = [t, UTType(filenameExtension: "psd") ?? .data]
        }
        guard p.runModal() == .OK, let url = p.url else { return }
        mode = .doc
        doc.open(url)
    }

    func newDocument() {
        mode = .doc
        doc.newDoc()
    }
}
