import SwiftUI
import AppKit
import ImageIO
import UniformTypeIdentifiers

let labelColors: [(name: String, color: Color)] = [
    ("red", .red), ("orange", .orange), ("yellow", .yellow),
    ("green", .green), ("blue", .blue), ("purple", .purple),
]

enum CompareMode: String, CaseIterable, Identifiable {
    case off, before, wipeV, wipeH, diff, mix
    var id: String { rawValue }
    var label: String {
        switch self {
        case .off: return "Off"
        case .before: return "Before"
        case .wipeV: return "Wipe Vertical"
        case .wipeH: return "Wipe Horizontal"
        case .diff: return "Difference"
        case .mix: return "50/50 Mix"
        }
    }
}

/// Inspector palettes, DaVinci color-page style: one at a time via the icon strip.
enum Palette: String, CaseIterable, Identifiable {
    case quick, meters, light, wheels, curves, zones, qualifier, windows, mixer,
         retouch, detail, fx, xform, versions
    var id: String { rawValue }
    var icon: String {
        switch self {
        case .quick: return "speedometer"
        case .meters: return "waveform.path.ecg"
        case .light: return "sun.max"
        case .wheels: return "circle.circle"
        case .curves: return "chart.line.uptrend.xyaxis"
        case .zones: return "square.split.bottomhalf.filled"
        case .qualifier: return "eyedropper.halffull"
        case .windows: return "circle.dashed"
        case .mixer: return "slider.horizontal.below.square.filled.and.square"
        case .retouch: return "bandage"
        case .detail: return "sparkle.magnifyingglass"
        case .fx: return "sparkles"
        case .xform: return "crop.rotate"
        case .versions: return "square.stack"
        }
    }
    var title: String {
        switch self {
        case .quick: return "Quick"
        case .meters: return "Meters"
        case .light: return "Light"
        case .wheels: return "Wheels"
        case .curves: return "Curves"
        case .zones: return "HDR Zones"
        case .qualifier: return "Qualifier"
        case .windows: return "Windows"
        case .mixer: return "Mixer"
        case .retouch: return "Retouch"
        case .detail: return "Detail"
        case .fx: return "Effects"
        case .xform: return "Transform"
        case .versions: return "Versions"
        }
    }
    /// darktable contextual hint: one line shown under the palette title.
    var hint: String {
        switch self {
        case .quick: return "Everyday controls in one place"
        case .meters: return "Drag vertically on the parade to set exposure"
        case .light: return "White balance, tonal range and colour"
        case .wheels: return "Drag inside a wheel to tint — the slider sets the mean"
        case .curves: return "Click to add a point, drag to move, double-click deletes"
        case .zones: return "Enable Image mode, then scroll on the photo to move the band under the cursor"
        case .qualifier: return "Key a colour range with the droppers, adjust inside it"
        case .windows: return "Tap or drag on the photo to place local masks"
        case .mixer: return "Channel mixing and monochrome conversion"
        case .retouch: return "Tap the photo with a tool armed — Esc backs out"
        case .detail: return "Sharpening, noise reduction and restoration"
        case .fx: return "Clarity, vignette, grain and light effects"
        case .xform: return "Rotation, keystone and crop"
        case .versions: return "Named snapshots and the edit history stack"
        }
    }
}

struct EditorView: View {
    let photo: Photo
    @EnvironmentObject var store: LibraryStore

    @State private var recipe = Recipe()
    @State private var rating = 0
    @State private var label = ""
    // sidecar values as loaded — guards the initial-bind onChange from
    // rewriting the sidecar file on every photo open
    @State private var loadedRating = -1
    @State private var loadedLabel = "\u{0}"
    @State private var image: CGImage?
    @State private var hist: [[UInt32]] = []
    @State private var wave: [UInt32] = []
    @State private var vec: [UInt32] = []
    @State private var cie: [UInt32] = []
    @State private var rendering = false
    /// monotonically increasing render id — stale detached results are dropped
    @State private var renderGen = 0
    @State private var dirty = false
    @State private var baseline = Recipe()
    @State private var status = ""
    @State private var renderTask: Task<Void, Never>?
    // tool: off|heal|dodge|burn|wbpick|clone|window|grad|flare|qpick|qadd|qsub
    @State private var retouchMode = "off"
    @State private var spotSize = 0.05
    @State private var lightRadius = 0.25
    @State private var lightEV = 0.5
    @State private var cloneRadius = 0.06
    @State private var pendingClone: [Double]? = nil
    @State private var cmp: CompareMode = .off
    @State private var wipePos: Double = 0.5
    @State private var baselineImg: CGImage?
    @State private var scopeKind = "parade"
    @State private var curveChan = 0
    // viewer
    @State private var zoom: CGFloat = 1
    @State private var pan = CGSize.zero
    @State private var stageHover = false
    // undo / redo (recipe snapshots, bursts coalesce at 0.8s)
    @State private var undoStack: [Recipe] = []
    @State private var redoStack: [Recipe] = []
    @State private var lastEditTime = Date.distantPast
    @State private var applyingHistory = false
    // palettes
    @State private var palette: Palette = .quick
    @State private var selWindow: UUID?
    // grade versions
    @State private var versions: [GradeVersion] = []
    @State private var baselineVersions: [GradeVersion] = []
    @State private var showVersionName = false
    @State private var versionName = ""
    // cursor probe / status bar (RawTherapee-style readout)
    @State private var probeText = ""
    @State private var probeBuf: [UInt8] = []
    @State private var probeW = 0
    @State private var probeH = 0
    @State private var lastHover: CGPoint = .zero
    @State private var stageSize: CGSize = .zero
    // darktable tone-equalizer image interaction: scroll on the photo to
    // adjust the EV band under the cursor
    @State private var zoneImgMode = false
    @State private var zoneHover: Int? = nil
    // scope-as-control drag state
    @State private var scopeDragBase: Double? = nil
    @State private var scopeTapTime = Date.distantPast
    // collapsible chrome (darktable panel-edge arrows)
    @State private var showInspector = true
    @State private var showStrip = true
    @State private var autoBusy = false

    /// letterboxed image rect inside the preview area (zoom/pan applied)
    private func imageRect(in size: CGSize) -> CGRect {
        guard let image else { return .zero }
        let iw = CGFloat(image.width), ih = CGFloat(image.height)
        let pad: CGFloat = 10
        let avail = CGSize(width: size.width - pad * 2, height: size.height - pad * 2)
        let sc = min(avail.width / iw, avail.height / ih) * zoom
        let w = iw * sc, h = ih * sc
        return CGRect(x: (size.width - w) / 2 + pan.width,
                      y: (size.height - h) / 2 + pan.height, width: w, height: h)
    }

    /// map a tap (view coords) into frame-normalized coords; crop-adjusted for
    /// frame-normalized (pre-crop) storages like spots/windows
    private func frameCoord(_ loc: CGPoint, in size: CGSize) -> (nx: Double, ny: Double, fx: Double, fy: Double)? {
        let rect = imageRect(in: size)
        guard rect.width > 0 else { return nil }
        let nx = Double((loc.x - rect.minX) / rect.width).clamped(to: 0...1)
        let ny = Double((loc.y - rect.minY) / rect.height).clamped(to: 0...1)
        let cl = recipe.crop[0], ct = recipe.crop[1]
        let sw = 1 - recipe.crop[0] - recipe.crop[2]
        let sh = 1 - recipe.crop[1] - recipe.crop[3]
        return (nx, ny, cl + nx * sw, ct + ny * sh)
    }

    private func placeAt(_ loc: CGPoint, in size: CGSize) {
        guard retouchMode != "off" else { return }
        guard let (nx, ny, fx, fy) = frameCoord(loc, in: size) else { return }
        switch retouchMode {
        case "heal":
            recipe.spots.append([fx, fy, spotSize, 0])
        case "dodge", "burn":
            recipe.lights.append([nx, ny, lightRadius, retouchMode == "dodge" ? lightEV : -lightEV])
        case "wbpick":
            recipe.wb_pick = [fx, fy]
            recipe.wb_mode = .pick
            retouchMode = "off"
        case "clone":
            if let s = pendingClone {
                recipe.clones.append([s[0], s[1], fx, fy, cloneRadius, 0])
                pendingClone = nil
            } else {
                pendingClone = [fx, fy]
                status = "Clone: tap destination"
            }
        case "window":
            var w = PowerWindow()
            w.kind = "circle"
            w.p = [fx, fy, 0.15, 0.15, 0, 0.4]
            recipe.windows.append(w)
            selWindow = w.id
        case "grad":
            var w = PowerWindow()
            w.kind = "gradient"
            w.p = [0.0, fy, 1.0, fy, 0, 0.5]
            recipe.windows.append(w)
            selWindow = w.id
        case "flare":
            recipe.flare[0] = nx
            recipe.flare[1] = ny
        case "qpick", "qadd", "qsub":
            if let c = samplePixel(nx: nx, ny: ny) {
                qualifierPick(c, mode: retouchMode)
            }
        default: break
        }
    }

    var body: some View {
        HSplitView {
            stageColumn
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            if showInspector {
                inspector
                    .frame(minWidth: 300, idealWidth: 316, maxWidth: 380)
            }
        }
        .background(Kou.bg0)
        .background(shortcutLayer)
        .task { load() }
        .onChange(of: recipe) { old, new in
            dirty = (new != baseline) || versions != baselineVersions
            store.unsavedEdits[photo.path] = dirty ? new : nil
            if applyingHistory {
                // programmatic recipe assignment (undo/redo/version apply)
                applyingHistory = false
            } else {
                recordUndo(old)
            }
            scheduleRender()
        }
        .onChange(of: versions) { _, _ in
            dirty = (recipe != baseline) || versions != baselineVersions
            store.unsavedVersions[photo.path] = (versions != baselineVersions) ? versions : nil
        }
        .onChange(of: cmp) { _, m in
            if m != .off { ensureBaseline() }
        }
        // Thumbnail hover-stars (and any other writer through
        // store.setRating) keep the open editor's rating mirror in sync —
        // otherwise a later save() would serialize the stale value back.
        .onChange(of: store.photos) { _, ps in
            if let p = ps.first(where: { $0.path == photo.path }), p.rating != rating {
                rating = p.rating
                loadedRating = p.rating
            }
        }
        .alert("Save Version", isPresented: $showVersionName) {
            TextField("Name", text: $versionName)
            Button("Save") { commitVersion() }
            Button("Cancel", role: .cancel) { versionName = "" }
        }
    }

    /// Global-ish keys via invisible command buttons (Cmd-modified keys only —
    /// bare digits/arrows go through the NSEvent monitor so text fields work).
    private var shortcutLayer: some View {
        Group {
            Button("") { undo() }.keyboardShortcut("z", modifiers: .command)
            Button("") { redo() }.keyboardShortcut("z", modifiers: [.command, .shift])
            Button("") { save() }.keyboardShortcut("s", modifiers: .command)
            Button("") { export() }.keyboardShortcut("e", modifiers: .command)
            Button("") { applyPrevRecipe() }.keyboardShortcut("=", modifiers: .command)
        }
        .opacity(0)
        .frame(width: 0, height: 0)
        .allowsHitTesting(false)
    }

    // MARK: stage (image + filmstrip)

    private var stageColumn: some View {
        VStack(spacing: 0) {
            GeometryReader { geo in
                ZStack {
                    Kou.bg0
                    if let image {
                        let rect = imageRect(in: geo.size)
                        // compare modes: baseline underneath (or alone)
                        if cmp != .off && cmp != .before {
                            if let baselineImg {
                                Image(baselineImg, scale: 1, label: Text("before"))
                                    .resizable()
                                    .frame(width: rect.width, height: rect.height)
                                    .position(x: rect.midX, y: rect.midY)
                            }
                        }
                        if cmp == .before {
                            if let baselineImg {
                                Image(baselineImg, scale: 1, label: Text(photo.name))
                                    .resizable()
                                    .frame(width: rect.width, height: rect.height)
                                    .position(x: rect.midX, y: rect.midY)
                                    .shadow(color: .black.opacity(0.6), radius: 12, y: 4)
                            } else {
                                ProgressView().tint(Kou.accent)
                            }
                        } else {
                            Image(image, scale: 1, label: Text(photo.name))
                                .resizable()
                                .frame(width: rect.width, height: rect.height)
                                .position(x: rect.midX, y: rect.midY)
                                .shadow(color: .black.opacity(0.6), radius: 12, y: 4)
                                .mask(wipeMask(in: rect))
                                .blendMode(cmp == .diff ? .difference : .normal)
                                .opacity(cmp == .mix ? 0.5 : 1)
                        }
                        Rectangle()
                            .stroke(Kou.border, lineWidth: 0.5)
                            .frame(width: rect.width, height: rect.height)
                            .position(x: rect.midX, y: rect.midY)
                        retouchMarkers(in: rect)
                        if cmp == .wipeV || cmp == .wipeH {
                            wipeDivider(in: rect)
                        }
                        if cmp != .off {
                            Text(cmp.label.uppercased())
                                .font(.system(size: 10, weight: .bold))
                                .tracking(1.5)
                                .padding(.horizontal, 8).padding(.vertical, 3)
                                .background(Capsule().fill(Kou.accent))
                                .foregroundStyle(Color.black.opacity(0.85))
                                .position(x: rect.minX + 46, y: rect.minY + 16)
                        }
                        if retouchMode != "off" {
                            toolBanner
                                .position(x: rect.midX, y: rect.maxY - 20)
                        }
                        // darktable tone-equalizer badge: hovering band + EV
                        if zoneImgMode, let zi = zoneHover {
                            Text(String(format: "%+d EV band   %+.2f", zi - 4, recipe.zones_ev[zi]))
                                .font(.system(size: 10, weight: .semibold).monospacedDigit())
                                .foregroundStyle(Kou.text1)
                                .padding(.horizontal, 9).padding(.vertical, 4)
                                .background(Capsule().fill(.black.opacity(0.72))
                                    .overlay(Capsule().stroke(Kou.accent.opacity(0.5), lineWidth: 0.5)))
                                .position(x: min(lastHover.x + 78, geo.size.width - 90),
                                          y: max(lastHover.y - 22, 14))
                                .allowsHitTesting(false)
                        }
                        // RawTherapee navigator: minimap + draggable viewport
                        if zoom > 1.01 {
                            navigator(in: rect, stage: geo.size)
                                .position(x: 12 + 75, y: 12 + 75 * rect.height / max(rect.width, 1) / 2)
                        }
                        zoomControls
                            // pinned to the stage edge — the image rect grows
                            // past the viewport once zoomed in
                            .position(x: geo.size.width - 96, y: geo.size.height - 24)
                    } else {
                        ProgressView()
                            .tint(Kou.accent)
                    }
                    // chrome toggles, darktable panel-edge arrows
                    HStack(spacing: 4) {
                        IconAction(icon: "rectangle.bottomhalf.filled", active: showStrip) {
                            showStrip.toggle()
                        }
                        .help("Filmstrip")
                        IconAction(icon: "sidebar.right", active: showInspector) {
                            withAnimation(.easeInOut(duration: 0.15)) { showInspector.toggle() }
                        }
                        .help("Inspector")
                    }
                    .padding(.horizontal, 6).padding(.vertical, 4)
                    .background(Capsule().fill(.black.opacity(0.6))
                        .overlay(Capsule().stroke(Kou.hairline, lineWidth: 0.5)))
                    .position(x: geo.size.width - 44, y: 18)
                }
                .contentShape(Rectangle())
                .onContinuousHover { phase in
                    switch phase {
                    case .active(let loc): updateProbe(loc, in: geo.size)
                    case .ended: stageHover = false; probeText = ""; zoneHover = nil
                    }
                }
                .onAppear { stageSize = geo.size }
                .onChange(of: geo.size) { _, s in stageSize = s }
                .gesture(SpatialTapGesture().onEnded { v in
                    placeAt(v.location, in: geo.size)
                })
                .gesture(DragGesture(minimumDistance: 4)
                    .onChanged { g in stageDrag(g, in: geo.size) }
                    .onEnded { _ in dragBase = nil })
                .gesture(MagnifyGesture()
                    .onChanged { v in
                        if !pinching { pinching = true; pinchBase = zoom }
                        zoom = (pinchBase * v.magnification).clamped(to: 0.5...8)
                        if zoom <= 1 { pan = .zero }
                    }
                    .onEnded { _ in pinching = false })
                .onTapGesture(count: 2) { _ in
                    // only reached when the spatial tap for tools didn't claim it
                    if retouchMode == "off" {
                        zoom = zoom > 1.01 ? 1 : 2
                        if zoom <= 1 { pan = .zero }
                    }
                }
                .onHover { stageHover = $0 }
            }
            if showStrip {
                filmstrip
            }
            statusBar
        }
        .onAppear {
            keyMon.handler = { [self] ev in handleKey(ev) }
            keyMon.install()
            scrollMon.handler = { [self] ev in
                guard stageHover else { return true }
                // darktable tone equalizer: scroll over the photo adjusts the
                // luminance band under the cursor instead of zooming
                if zoneImgMode, let zi = zoneIndex(at: lastHover) {
                    let d = Double(ev.scrollingDeltaY + ev.scrollingDeltaX)
                    if d != 0 {
                        var z = recipe.zones_ev
                        z[zi] = (z[zi] + d * 0.03).clamped(to: -4...4)
                        recipe.zones_ev = z
                        zoneHover = zi
                    }
                    return false
                }
                let nz = (zoom * (1 + ev.scrollingDeltaY * 0.003)).clamped(to: 0.5...8)
                if nz != zoom { zoom = nz; if zoom <= 1 { pan = .zero } }
                return true
            }
            scrollMon.install()
        }
        .onDisappear {
            keyMon.uninstall()
            scrollMon.uninstall()
        }
    }

    @State private var pinching = false
    @State private var pinchBase: CGFloat = 1
    @State private var dragBase: [Double]?
    @State private var keyMon = KeyMonitor()
    @State private var scrollMon = ScrollMonitor()

    /// wipe divider line + drag handle
    private func wipeDivider(in rect: CGRect) -> some View {
        let isV = cmp == .wipeV
        let pos = CGFloat(wipePos)
        let line: Path = isV
            ? Path { p in p.move(to: .init(x: rect.minX + pos * rect.width, y: rect.minY))
                          p.addLine(to: .init(x: rect.minX + pos * rect.width, y: rect.maxY)) }
            : Path { p in p.move(to: .init(x: rect.minX, y: rect.minY + pos * rect.height))
                          p.addLine(to: .init(x: rect.maxX, y: rect.minY + pos * rect.height)) }
        return ZStack {
            line.stroke(.white.opacity(0.85), lineWidth: 1.5)
            // invisible fat handle along the divider for dragging
            if isV {
                Rectangle().fill(.white.opacity(0.001))
                    .frame(width: 18, height: rect.height)
                    .position(x: rect.minX + pos * rect.width, y: rect.midY)
                    .gesture(DragGesture(minimumDistance: 0).onChanged { g in
                        wipePos = Double((g.location.x - rect.minX) / rect.width).clamped(to: 0.02...0.98)
                    })
            } else {
                Rectangle().fill(.white.opacity(0.001))
                    .frame(width: rect.width, height: 18)
                    .position(x: rect.midX, y: rect.minY + pos * rect.height)
                    .gesture(DragGesture(minimumDistance: 0).onChanged { g in
                        wipePos = Double((g.location.y - rect.minY) / rect.height).clamped(to: 0.02...0.98)
                    })
            }
        }
    }

    /// mask for the edited image, in the image's own layout space: the
    /// divider's "before" half is knocked out so the baseline shows through.
    private func wipeMask(in rect: CGRect) -> some View {
        GeometryReader { g in
            ZStack(alignment: .topLeading) {
                Color.white
                if cmp == .wipeV {
                    Color.black
                        .frame(width: g.size.width * CGFloat(wipePos), height: g.size.height)
                } else if cmp == .wipeH {
                    Color.black
                        .frame(width: g.size.width, height: g.size.height * CGFloat(wipePos))
                }
            }
        }
    }

    private var zoomControls: some View {
        HStack(spacing: 4) {
            IconAction(icon: "arrow.down.right.and.arrow.up.left") { zoom = 1; pan = .zero }
            IconAction(icon: "minus") { zoom = max(0.5, zoom - 0.25); if zoom <= 1 { pan = .zero } }
            Text(String(format: "%.0f%%", zoom * 100))
                .font(.system(size: 10).monospacedDigit())
                .foregroundStyle(Kou.text1)
                .frame(width: 34)
            IconAction(icon: "plus") { zoom = min(8, zoom + 0.25) }
            IconAction(icon: "1.magnifyingglass") { zoom = min(8, 1400 / CGFloat(image?.width ?? 1400)) }
        }
        .padding(.horizontal, 6).padding(.vertical, 4)
        .background(Capsule().fill(.black.opacity(0.6))
            .overlay(Capsule().stroke(Kou.hairline, lineWidth: 0.5)))
    }

    /// Stage drag routing: window move / gradient draw / pan when zoomed.
    private func stageDrag(_ g: DragGesture.Value, in size: CGSize) {
        if retouchMode == "window" || retouchMode == "grad" {
            guard let (_, _, sfx, sfy) = frameCoord(g.startLocation, in: size),
                  let (_, _, fx, fy) = frameCoord(g.location, in: size) else { return }
            if retouchMode == "grad" {
                // drag defines the gradient line start→current
                if let i = recipe.windows.lastIndex(where: { $0.id == selWindow })
                    ?? recipe.windows.indices.last {
                    recipe.windows[i].kind = "gradient"
                    recipe.windows[i].p = [sfx, sfy, fx, fy, 0, 0.35]
                    selWindow = recipe.windows[i].id
                }
                return
            }
            // move the selected (or newest) window
            guard let i = recipe.windows.lastIndex(where: { $0.id == selWindow })
                ?? recipe.windows.indices.last else { return }
            if dragBase == nil { dragBase = recipe.windows[i].p }
            guard let base = dragBase else { return }
            var p = base
            let dfx = fx - sfx, dfy = fy - sfy
            if recipe.windows[i].kind == "gradient" {
                p[0] = base[0] + dfx; p[1] = base[1] + dfy
                p[2] = base[2] + dfx; p[3] = base[3] + dfy
            } else {
                p[0] = (base[0] + dfx).clamped(to: 0...1)
                p[1] = (base[1] + dfy).clamped(to: 0...1)
            }
            recipe.windows[i].p = p
        } else if retouchMode == "off" && zoom > 1.001 {
            if dragBase == nil { dragBase = [Double(pan.width), Double(pan.height)] }
            guard let base = dragBase, base.count == 2 else { return }
            pan = CGSize(width: base[0] + g.translation.width,
                         height: base[1] + g.translation.height)
        }
    }

    /// Bare-key shortcuts. Returns true when the key was consumed.
    private func handleKey(_ ev: NSEvent) -> Bool {
        // let text fields own the keyboard
        if let fr = ev.window?.firstResponder, fr is NSTextView || fr is NSTextField {
            return false
        }
        guard ev.modifierFlags.intersection(.deviceIndependentFlagsMask)
            .isDisjoint(with: [.command, .control, .option]) else { return false }
        if let c = ev.charactersIgnoringModifiers?.lowercased().first {
            switch c {
            case "0": setRating(0); return true
            case "1"..."5": setRating(Int(String(c))!); return true
            case "b": cmp = (cmp == .before ? .off : .before); return true
            case "w":
                cmp = cmp == .wipeV ? .wipeH : cmp == .wipeH ? .off : .wipeV
                return true
            default: break
            }
        }
        switch ev.keyCode {
        case 123: stepPhoto(-1); return true    // ←
        case 124: stepPhoto(1); return true     // →
        case 53:                                // esc
            if retouchMode != "off" { retouchMode = "off"; return true }
            if cmp != .off { cmp = .off; return true }
            return false
        default: return false
        }
    }

    @ViewBuilder
    private var toolBanner: some View {
        let names: [String: String] = [
            "heal": "Heal — tap blemishes", "clone": pendingClone == nil ? "Clone — tap source" : "Clone — tap destination",
            "dodge": "Dodge — tap to lighten", "burn": "Burn — tap to darken",
            "wbpick": "Pick WB — tap a neutral point", "window": "Window — tap to add, drag to move",
            "grad": "Gradient — drag to draw the line", "flare": "Flare — tap light position",
            "qpick": "Qualifier — tap a colour to key it",
            "qadd": "Qualifier + — tap to add to the key",
            "qsub": "Qualifier − — tap to remove from the key",
        ]
        Text(names[retouchMode] ?? retouchMode)
            .font(.system(size: 10.5, weight: .medium))
            .foregroundStyle(Kou.text1)
            .padding(.horizontal, 12).padding(.vertical, 6)
            .background(Capsule().fill(.black.opacity(0.72))
                .overlay(Capsule().stroke(Kou.accent.opacity(0.5), lineWidth: 0.5)))
    }

    /// Cursor probe: sample the small sRGB probe buffer under the pointer
    /// and format an RT-style readout. Also feeds the zone-EQ badge.
    private func updateProbe(_ loc: CGPoint, in size: CGSize) {
        stageHover = true
        lastHover = loc
        let rect = imageRect(in: size)
        guard rect.contains(loc), probeW > 0, !probeBuf.isEmpty else {
            probeText = ""; zoneHover = nil; return
        }
        let nx = Double((loc.x - rect.minX) / rect.width)
        let ny = Double((loc.y - rect.minY) / rect.height)
        let px = min(max(Int(nx * Double(probeW)), 0), probeW - 1)
        let py = min(max(Int(ny * Double(probeH)), 0), probeH - 1)
        let o = (py * probeW + px) * 4
        guard o + 2 < probeBuf.count else { return }
        let r = Double(probeBuf[o]), g = Double(probeBuf[o + 1]), b = Double(probeBuf[o + 2])
        let (hh, ss, vv) = rgbHsv((r / 255, g / 255, b / 255))
        probeText = String(format: "%4d,%4d  ·  R%3d G%3d B%3d  ·  H%3.0f° S%2.0f%% V%2.0f%%",
                           Int(nx * Double(image?.width ?? 0)), Int(ny * Double(image?.height ?? 0)),
                           Int(r), Int(g), Int(b), hh * 360, ss * 100, vv * 100)
        if zoneImgMode { zoneHover = zoneIndex(luma: 0.2126 * r + 0.7152 * g + 0.0722 * b) }
    }

    /// sRGB luma byte → tone-equalizer band index 0...8 (centres −4…+4 EV).
    private func zoneIndex(luma l: Double) -> Int {
        min(max(Int((log2(max(l / 255, 0.015)) + 4).rounded()), 0), 8)
    }

    private func zoneIndex(at loc: CGPoint) -> Int? {
        guard let (nx, ny, _, _) = frameCoord(loc, in: stageSize),
              nx >= 0, nx <= 1, ny >= 0, ny <= 1,
              probeW > 0 else { return nil }
        let px = min(max(Int(nx * Double(probeW)), 0), probeW - 1)
        let py = min(max(Int(ny * Double(probeH)), 0), probeH - 1)
        let o = (py * probeW + px) * 4
        guard o + 2 < probeBuf.count else { return nil }
        return zoneIndex(luma: 0.2126 * Double(probeBuf[o]) + 0.7152 * Double(probeBuf[o + 1])
            + 0.0722 * Double(probeBuf[o + 2]))
    }

    /// RawTherapee navigator: mini preview + draggable viewport rectangle,
    /// shown top-left whenever zoomed in.
    private func navigator(in rect: CGRect, stage: CGSize) -> some View {
        let nw: CGFloat = 150
        let nh = nw * rect.height / max(rect.width, 1)
        let fx0 = Double((0 - rect.minX) / rect.width).clamped(to: 0...1)
        let fx1 = Double((stage.width - rect.minX) / rect.width).clamped(to: 0...1)
        let fy0 = Double((0 - rect.minY) / rect.height).clamped(to: 0...1)
        let fy1 = Double((stage.height - rect.minY) / rect.height).clamped(to: 0...1)
        return ZStack(alignment: .topLeading) {
            if let image {
                Image(image, scale: 1, label: Text("navigator"))
                    .resizable().frame(width: nw, height: nh).opacity(0.8)
            }
            Rectangle()
                .fill(Color.white.opacity(0.07))
                .overlay(Rectangle().stroke(Kou.accent, lineWidth: 1.2))
                .frame(width: max(CGFloat(fx1 - fx0) * nw, 6),
                       height: max(CGFloat(fy1 - fy0) * nh, 6))
                .offset(x: CGFloat(fx0) * nw, y: CGFloat(fy0) * nh)
        }
        .frame(width: nw, height: nh)
        .background(Kou.bg1.opacity(0.9))
        .clipShape(RoundedRectangle(cornerRadius: 6))
        .overlay(RoundedRectangle(cornerRadius: 6).stroke(Kou.border, lineWidth: 0.75))
        .shadow(color: .black.opacity(0.5), radius: 6, y: 2)
        .contentShape(Rectangle())
        .gesture(DragGesture(minimumDistance: 0).onChanged { g in
            // centre the viewport on the dragged point
            let fx = Double(g.location.x / nw).clamped(to: 0...1)
            let fy = Double(g.location.y / nh).clamped(to: 0...1)
            pan = CGSize(
                width: stage.width / 2 - CGFloat(fx) * rect.width - (stage.width - rect.width) / 2,
                height: stage.height / 2 - CGFloat(fy) * rect.height - (stage.height - rect.height) / 2)
        })
    }

    /// Bottom status strip (RawTherapee): probe readout / filename, render
    /// spinner and zoom readout — always visible, no inspector needed.
    private var statusBar: some View {
        VStack(spacing: 0) {
            Kou.hairline.frame(height: 1)
            HStack(spacing: 10) {
                Text(probeText.isEmpty ? photo.name : probeText)
                    .font(.system(size: 9.5).monospacedDigit())
                    .foregroundStyle(probeText.isEmpty ? Kou.text3 : Kou.text2)
                    .lineLimit(1).truncationMode(.middle)
                Spacer()
                if rendering {
                    ProgressView().controlSize(.mini).tint(Kou.accent)
                        .frame(width: 10, height: 10)
                }
                Text(zoneImgMode ? "zone scroll" : String(format: "%.0f%%", zoom * 100))
                    .font(.system(size: 9.5).monospacedDigit())
                    .foregroundStyle(zoneImgMode ? Kou.accent : Kou.text3)
            }
            .padding(.horizontal, 10).padding(.vertical, 4)
            .background(Kou.bg1)
        }
    }

    private var filmstrip: some View {
        VStack(spacing: 0) {
            Kou.hairline.frame(height: 1)
            ScrollViewReader { proxy in
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 6) {
                        ForEach(store.filtered) { p in
                            FilmCell(photo: p, selected: p.path == store.selection?.path,
                                     dirty: store.unsavedEdits[p.path] != nil)
                                .onTapGesture { store.selection = p }
                                .id(p.id)
                        }
                    }
                    .padding(.horizontal, 10).padding(.vertical, 8)
                }
                .frame(height: 74)
                .onAppear { proxy.scrollTo(photo.id, anchor: .center) }
            }
            .background(Kou.bg1)
        }
    }

    // MARK: inspector

    private var inspector: some View {
        VStack(spacing: 0) {
            // header: filename, stars, labels, undo, compare
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 8) {
                    Text(photo.name)
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(Kou.text1)
                        .lineLimit(1).truncationMode(.middle)
                    if dirty {
                        Circle().fill(Kou.accent).frame(width: 6, height: 6)
                    }
                    Spacer()
                    Stars(rating: $rating)
                        .onChange(of: rating) { _, r in
                            guard r != loadedRating else { return }
                            loadedRating = r
                            store.setRating(path: photo.path, r)
                        }
                }
                HStack(spacing: 6) {
                    LabelPicker(label: $label)
                        .onChange(of: label) { _, l in
                            guard l != loadedLabel else { return }
                            loadedLabel = l
                            KouEngine.shared.setLabel(path: photo.path, l)
                            if let i = store.photos.firstIndex(where: { $0.path == photo.path }) {
                                store.photos[i].label = l
                            }
                        }
                    Spacer()
                    IconAction(icon: "arrow.uturn.backward") { undo() }
                        .opacity(undoStack.isEmpty ? 0.35 : 1)
                    IconAction(icon: "arrow.uturn.forward") { redo() }
                        .opacity(redoStack.isEmpty ? 0.35 : 1)
                    Menu {
                        ForEach(CompareMode.allCases) { m in
                            Button {
                                cmp = m
                                if m != .off { ensureBaseline() }
                            } label: {
                                if cmp == m {
                                    Label(m.label, systemImage: "checkmark")
                                } else {
                                    Text(m.label)
                                }
                            }
                        }
                    } label: {
                        HStack(spacing: 4) {
                            Image(systemName: "rectangle.split.2x1")
                                .font(.system(size: 11, weight: .medium))
                            Text(cmp == .off ? "Compare" : cmp.label)
                                .font(.system(size: 10.5, weight: .medium))
                        }
                        .foregroundStyle(cmp == .off ? Kou.text2 : Kou.accent)
                        .padding(.horizontal, 8).padding(.vertical, 5)
                        .background(RoundedRectangle(cornerRadius: 6)
                            .fill(cmp == .off ? Kou.bg3 : Kou.accentSoft)
                            .overlay(RoundedRectangle(cornerRadius: 6).stroke(
                                cmp == .off ? Kou.hairline : Kou.accent.opacity(0.4), lineWidth: 0.5)))
                    }
                    .menuStyle(.borderlessButton).menuIndicator(.hidden).fixedSize()
                }
            }
            .padding(.horizontal, 12).padding(.vertical, 10)
            .background(Kou.bg1)
            .overlay(alignment: .bottom) { Kou.hairline.frame(height: 1) }

            paletteStrip

            // palette header: title + hint + per-palette reset
            VStack(alignment: .leading, spacing: 2) {
                HStack {
                    Text(palette.title.uppercased())
                        .font(.system(size: 9.5, weight: .semibold)).tracking(1.2)
                        .foregroundStyle(Kou.text2)
                    Spacer()
                    if paletteDirty(palette) {
                        Button {
                            resetPalette(palette)
                        } label: {
                            Image(systemName: "arrow.counterclockwise")
                                .font(.system(size: 9.5, weight: .bold))
                                .foregroundStyle(Kou.accent)
                        }
                        .buttonStyle(.plain)
                        .help("Reset this palette")
                    }
                }
                Text(palette.hint)
                    .font(.system(size: 8.5))
                    .foregroundStyle(Kou.text3)
                    .lineLimit(2).fixedSize(horizontal: false, vertical: true)
            }
            .padding(.horizontal, 12).padding(.vertical, 6)
            .background(Kou.bg1)
            .overlay(alignment: .bottom) { Kou.hairline.frame(height: 1) }
            ScrollView {
                VStack(alignment: .leading, spacing: 10) {
                    paletteContent
                    Color.clear.frame(height: 4)
                }
                .padding(10)
            }
            .scrollIndicators(.visible)

            actionBar
        }
        .background(Kou.bg1)
        .overlay(alignment: .leading) { Kou.hairline.frame(width: 1) }
    }

    /// DaVinci palette tab strip: icon per palette, underline when active,
    /// amber dot top-right when that section has non-default values.
    private var paletteStrip: some View {
        HStack(spacing: 0) {
            ForEach(Palette.allCases) { p in
                Button {
                    palette = p
                    if p == .windows || p == .qualifier { /* keep tool context */ }
                } label: {
                    VStack(spacing: 2) {
                        ZStack(alignment: .topTrailing) {
                            Image(systemName: p.icon)
                                .font(.system(size: 12))
                                .foregroundStyle(palette == p ? Kou.accent : Kou.text2)
                                .frame(width: 18, height: 16)
                            if paletteDirty(p) {
                                Circle().fill(Kou.accent)
                                    .frame(width: 4, height: 4)
                                    .offset(x: 3, y: -2)
                            }
                        }
                        Rectangle()
                            .fill(palette == p ? Kou.accent : .clear)
                            .frame(height: 2)
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 5)
                    .background(palette == p ? Kou.bg2 : .clear)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .help(p.title)
            }
        }
        .padding(.horizontal, 4)
        .background(Kou.bg1)
        .overlay(alignment: .bottom) { Kou.hairline.frame(height: 1) }
    }

    @ViewBuilder
    private var paletteContent: some View {
        switch palette {
        case .quick: quickPalette
        case .meters: metersPalette
        case .light: lightPalette
        case .wheels: wheelsPalette
        case .curves: curvesPalette
        case .zones: zonesPalette
        case .qualifier: qualifierPalette
        case .windows: windowsPalette
        case .mixer: mixerPalette
        case .retouch: retouchPalette
        case .detail: detailPalette
        case .fx: fxPalette
        case .xform: xformPalette
        case .versions: versionsPalette
        }
    }

    /// darktable quick-access panel: the 20% of controls used for 80% of
    /// edits, curated in one flat scroll — histogram, WB, tone, colour.
    private var quickPalette: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("Histogram") {
                if !hist.isEmpty {
                    HistogramView(hist: hist).frame(height: 72)
                } else {
                    Rectangle().fill(Kou.bg3).frame(height: 72)
                        .overlay(ProgressView().tint(Kou.text3))
                }
            }
            Panel("White Balance", trailing: {
                ToolChip(label: "Pick", icon: "eyedropper", active: retouchMode == "wbpick") {
                    retouchMode = retouchMode == "wbpick" ? "off" : "wbpick"
                }
            }) {
                SegPicker([(WbMode.asShot, "As Shot"), (.auto, "Auto"),
                           (.manual, "Manual"), (.pick, "Pick")],
                          selection: $recipe.wb_mode)
                SliderRow("Temp", $recipe.temperature, -1...1, track: Kou.tempTrack)
                SliderRow("Tint", $recipe.tint, -1...1, track: Kou.tintTrack)
            }
            Panel("Tone", trailing: {
                ToolChip(label: "Auto", icon: "wand.and.stars") { runAuto(.all) }
                    .help("Analyze the photo and apply every suggestion")
                    .opacity(autoBusy ? 0.5 : 1)
            }) {
                SliderRow("Exposure", $recipe.exposure, -4...4, step: 0.05)
                SliderRow("Contrast", $recipe.contrast, -1...1)
                SliderRow("Highlights", $recipe.highlights, -1...1)
                SliderRow("Shadows", $recipe.shadows, -1...1)
                SliderRow("Whites", $recipe.whites, -1...1)
                SliderRow("Blacks", $recipe.blacks, -1...1)
            }
            Panel("Colour", trailing: {
                ToolChip(label: "Auto", icon: "wand.and.stars") { runAuto(.colour) }
                    .help("Estimate vibrance from the chroma distribution")
                    .opacity(autoBusy ? 0.5 : 1)
            }) {
                SliderRow("Saturation", $recipe.saturation, -1...1)
                SliderRow("Vibrance", $recipe.vibrance, -1...1)
                SliderRow("Clarity", $recipe.clarity, -1...1)
            }
            Panel("Geometry", trailing: {
                ToolChip(label: "Auto", icon: "wand.and.stars") { runAuto(.geometry) }
                    .help("Detect horizon tilt and converging verticals")
                    .opacity(autoBusy ? 0.5 : 1)
            }) {
                SliderRow("Straighten", $recipe.rotation_deg, -10...10, step: 0.1)
            }
        }
    }

    private var metersPalette: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("Histogram") {
                if !hist.isEmpty {
                    HistogramView(hist: hist).frame(height: 84)
                } else {
                    Rectangle().fill(Kou.bg3).frame(height: 84)
                        .overlay(ProgressView().tint(Kou.text3))
                }
            }
            Panel("Scopes") {
                SegPicker([("parade", "Parade"), ("vector", "Vector"), ("cie", "CIE")],
                          selection: $scopeKind)
                ZStack {
                    ScopesView(wave: wave, vec: vec, cie: cie, kind: scopeKind)
                    // darktable: the waveform itself is a control — vertical
                    // drag sets exposure, double-click resets it. The
                    // minimumDistance:0 drag swallows onTapGesture, so the
                    // double-click is detected manually like in TrackSlider.
                    if scopeKind == "parade" {
                        Color.white.opacity(0.001)
                            .contentShape(Rectangle())
                            .gesture(DragGesture(minimumDistance: 0)
                                .onChanged { g in
                                    if scopeDragBase == nil {
                                        scopeDragBase = recipe.exposure
                                        if g.time.timeIntervalSince(scopeTapTime) < 0.3 {
                                            recipe.exposure = 0
                                            scopeDragBase = 0
                                            scopeTapTime = .distantPast
                                            return
                                        }
                                        scopeTapTime = g.time
                                    }
                                    recipe.exposure = ((scopeDragBase ?? 0)
                                        - Double(g.translation.height) * 0.015)
                                        .clamped(to: -4...4)
                                }
                                .onEnded { _ in scopeDragBase = nil })
                    }
                }
                .frame(height: 128)
                Text(scopeKind == "parade"
                     ? "Drag up/down on the parade = exposure · double-click resets"
                     : "Switch to Parade for drag-to-expose")
                    .font(.system(size: 8.5)).foregroundStyle(Kou.text3)
            }
        }
    }

    private var lightPalette: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("White Balance", trailing: {
                ToolChip(label: "Pick", icon: "eyedropper", active: retouchMode == "wbpick") {
                    retouchMode = retouchMode == "wbpick" ? "off" : "wbpick"
                }
            }) {
                SegPicker([(WbMode.asShot, "As Shot"), (.auto, "Auto"),
                           (.manual, "Manual"), (.pick, "Pick")],
                          selection: $recipe.wb_mode)
                if recipe.wb_mode == .pick || retouchMode == "wbpick" {
                    SliderRow("Pick Area", $recipe.wb_pick_size, 0.002...0.2, reset: 0.025)
                        .help("Sampling half-width of the WB eyedropper as a fraction of the frame")
                }
                SliderRow("Temp", $recipe.temperature, -1...1, track: Kou.tempTrack)
                SliderRow("Tint", $recipe.tint, -1...1, track: Kou.tintTrack)
            }
            Panel("Tone", trailing: {
                ToolChip(label: "Auto", icon: "wand.and.stars") {
                    recipe.wb_mode = .auto
                    recipe.auto_exposure = true
                    recipe.auto_contrast = true
                    status = "Auto: WB + exposure + contrast"
                }
            }) {
                HStack(spacing: 6) {
                    Toggle("Auto Exp", isOn: $recipe.auto_exposure)
                        .font(.system(size: 10)).foregroundStyle(Kou.text2)
                        .controlSize(.mini)
                    Toggle("Auto Contrast", isOn: $recipe.auto_contrast)
                        .font(.system(size: 10)).foregroundStyle(Kou.text2)
                        .controlSize(.mini)
                }
                SliderRow("Exposure", $recipe.exposure, -4...4, step: 0.05)
                SliderRow("Contrast", $recipe.contrast, -1...1)
                SliderRow("Pivot", $recipe.pivot, 0.05...0.5, reset: 0.18)
                SliderRow("Highlights", $recipe.highlights, -1...1)
                SliderRow("Shadows", $recipe.shadows, -1...1)
                SliderRow("Whites", $recipe.whites, -1...1)
                SliderRow("Blacks", $recipe.blacks, -1...1)
                SliderRow("HL Roll", $recipe.highlight_rolloff, 0.5...2, reset: 1.0)
                SliderRow("SH Roll", $recipe.shadow_rolloff, 0.5...2, reset: 1.0)
            }
            Panel("Color") {
                SliderRow("Saturation", $recipe.saturation, -1...1)
                SliderRow("Vibrance", $recipe.vibrance, -1...1)
            }
        }
    }

    private var wheelsPalette: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("Primaries", trailing: { LookPicker(look: $recipe.look, photo: photo, recipe: recipe) }) {
                HStack(spacing: 6) {
                    ColorWheel(title: "Lift", v: $recipe.lift, center: 0)
                    ColorWheel(title: "Gamma", v: $recipe.gamma, center: 1)
                    ColorWheel(title: "Gain", v: $recipe.gain, center: 1)
                    ColorWheel(title: "Offset", v: $recipe.offset, center: 0)
                }
                HStack(spacing: 6) {
                    Text("LUT")
                        .font(.system(size: 10.5, weight: .medium))
                        .foregroundStyle(Kou.text2)
                        .frame(width: 60, alignment: .leading)
                    ToolChip(label: recipe.lut_file.isEmpty
                             ? "Choose .cube…"
                             : URL(fileURLWithPath: recipe.lut_file)
                                .deletingPathExtension().lastPathComponent,
                             icon: "doc.badge.plus") { pickLut() }
                    if !recipe.lut_file.isEmpty {
                        ToolChip(label: "", icon: "xmark") { recipe.lut_file = "" }
                            .help("Clear LUT")
                    }
                    Spacer()
                }
                if !recipe.lut_file.isEmpty {
                    SliderRow("Amount", $recipe.lut_amount, 0...1, reset: 1)
                }
            }
            Panel("Split Tone") {
                SliderRow("Shd Hue", $recipe.shadow_hue, 0...1, reset: 0.55, track: Kou.hueTrack)
                SliderRow("Shd Sat", $recipe.shadow_sat, 0...1)
                SliderRow("Mid Hue", $recipe.midtone_hue, 0...1, reset: 0.55, track: Kou.hueTrack)
                SliderRow("Mid Sat", $recipe.midtone_sat, 0...1)
                SliderRow("Hi Hue", $recipe.highlight_hue, 0...1, reset: 0.08, track: Kou.hueTrack)
                SliderRow("Hi Sat", $recipe.highlight_sat, 0...1)
            }
        }
    }

    private var curvesPalette: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("Curves") {
                SegPicker([(0, "Y"), (1, "R"), (2, "G"), (3, "B"),
                           (4, "H·H"), (5, "H·S"), (6, "H·L"), (7, "L·S"), (8, "S·S")],
                          selection: $curveChan)
                CurveEditor(points: curveBinding(curveChan),
                            tint: curveTint(curveChan), hist: hist.count > 3 ? hist[3] : [],
                            spectrum: curveChan >= 4 && curveChan <= 6)
                    .frame(height: 132)
                HStack {
                    ToolChip(label: "Clear", icon: "xmark") {
                        curveBinding(curveChan).wrappedValue = []
                    }
                    Spacer()
                    Text(curveName(curveChan))
                        .font(.system(size: 9.5)).foregroundStyle(Kou.text3)
                }
            }
        }
    }

    private var zonesPalette: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("Tone Equalizer", trailing: {
                HStack(spacing: 4) {
                    ToolChip(label: "Auto", icon: "wand.and.stars") { runAuto(.zones) }
                        .help("Set band gains from the luma histogram")
                        .opacity(autoBusy ? 0.5 : 1)
                    ToolChip(label: "Image", icon: "hand.draw", active: zoneImgMode) {
                        zoneImgMode.toggle()
                    }
                    .help("Scroll on the photo to adjust the band under the cursor")
                    ToolChip(label: "Reset", icon: "arrow.counterclockwise") {
                        recipe.zones_ev = [Double](repeating: 0, count: 9)
                    }
                }
            }) {
                ZoneEQ(zones: $recipe.zones_ev)
                Text(zoneImgMode
                     ? "Hover the photo and scroll — the EV badge marks the band under the cursor"
                     : "EV gain per luminance band (−4…+4 EV around mid grey)")
                    .font(.system(size: 8.5)).foregroundStyle(zoneImgMode ? Kou.accent : Kou.text3)
            }
            Panel("HDR Zones") {
                ZoneRow("Dark", $recipe.z_dark)
                ZoneRow("Shadow", $recipe.z_shadow)
                ZoneRow("Light", $recipe.z_light)
                ZoneRow("Global", $recipe.z_global)
            }
        }
    }

    /// DaVinci qualifier: eyedroppers + HSL gradient range bars + finesse.
    private var qualifierPalette: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("Qualifier", trailing: {
                Toggle("", isOn: $recipe.q_enabled)
                    .labelsHidden().controlSize(.mini).tint(Kou.accent)
                    .onChange(of: recipe.q_enabled) { _, on in
                        if !on { recipe.qh[1] = 0; recipe.q_show = false }
                        else if recipe.qh[1] == 0 { recipe.qh[1] = 0.1 }
                    }
            }) {
                // eyedropper row: pick / add / subtract
                HStack(spacing: 6) {
                    Text("Pick").font(.system(size: 10.5)).foregroundStyle(Kou.text2)
                    ToolChip(label: "New", icon: "eyedropper",
                             active: retouchMode == "qpick") {
                        retouchMode = retouchMode == "qpick" ? "off" : "qpick"
                    }
                    ToolChip(label: "+", icon: "plus.circle",
                             active: retouchMode == "qadd") {
                        retouchMode = retouchMode == "qadd" ? "off" : "qadd"
                    }
                    ToolChip(label: "−", icon: "minus.circle",
                             active: retouchMode == "qsub") {
                        retouchMode = retouchMode == "qsub" ? "off" : "qsub"
                    }
                    Spacer()
                    ToolChip(label: "View", icon: "eye", active: recipe.q_show) {
                        recipe.q_show.toggle()
                    }
                    .help("Highlight: show the matte — keyed area in colour, rest grey")
                }
                if recipe.q_enabled {
                    // HSL gradient bars
                    RangeBar(title: "Hue",
                             lo: Binding(
                                 get: { (recipe.qh[0] - recipe.qh[1]).clamped(to: 0...1) },
                                 set: { v in
                                     let hi = (recipe.qh[0] + recipe.qh[1]).clamped(to: 0...1)
                                     recipe.qh[0] = (v + hi) / 2
                                     recipe.qh[1] = max(0.002, (hi - v) / 2)
                                 }),
                             hi: Binding(
                                 get: { (recipe.qh[0] + recipe.qh[1]).clamped(to: 0...1) },
                                 set: { v in
                                     let lo = (recipe.qh[0] - recipe.qh[1]).clamped(to: 0...1)
                                     recipe.qh[0] = (lo + v) / 2
                                     recipe.qh[1] = max(0.002, (v - lo) / 2)
                                 }),
                             gradient: LinearGradient(
                                 colors: (0...12).map { Color(hue: Double($0) / 12, saturation: 0.8, brightness: 0.9) },
                                 startPoint: .leading, endPoint: .trailing))
                    RangeBar(title: "Sat",
                             lo: $recipe.qs[0], hi: $recipe.qs[1],
                             gradient: LinearGradient(colors: [.gray.opacity(0.4), .orange],
                                                      startPoint: .leading, endPoint: .trailing))
                    RangeBar(title: "Lum",
                             lo: $recipe.ql[0], hi: $recipe.ql[1],
                             gradient: LinearGradient(colors: [.black, .white],
                                                      startPoint: .leading, endPoint: .trailing))
                    SliderRow("Hue Soft", $recipe.qh[2], 0.01...0.4, reset: 0.1)
                    SliderRow("Sat Soft", $recipe.qs[2], 0.01...0.4, reset: 0.1)
                    SliderRow("Lum Soft", $recipe.ql[2], 0.01...0.4, reset: 0.1)
                    Text("MATTE FINESSE")
                        .font(.system(size: 8.5, weight: .semibold)).tracking(1.2)
                        .foregroundStyle(Kou.text3)
                    SliderRow("Clean Blk", $recipe.q_clean[0], 0...1)
                    SliderRow("Clean Wht", $recipe.q_clean[1], 0...1, reset: 1)
                    SliderRow("Blur", $recipe.q_blur, 0...1)
                    HStack {
                        Text("Invert mask").font(.system(size: 10.5)).foregroundStyle(Kou.text2)
                        Spacer()
                        Toggle("", isOn: $recipe.q_invert)
                            .labelsHidden().controlSize(.mini).tint(Kou.accent)
                    }
                    Text("ADJUST INSIDE KEY")
                        .font(.system(size: 8.5, weight: .semibold)).tracking(1.2)
                        .foregroundStyle(Kou.text3)
                    SliderRow("Hue Δ", $recipe.qadj[0], -0.5...0.5, track: Kou.hueTrack)
                    SliderRow("Sat Δ", $recipe.qadj[1], -1...1)
                    SliderRow("Lum Δ", $recipe.qadj[2], -1...1)
                    SliderRow("Temp Δ", $recipe.qadj[3], -1...1)
                }
            }
        }
    }

    private var windowsPalette: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("Power Windows", trailing: {
                HStack(spacing: 4) {
                    ToolChip(label: "Circle", icon: "plus.circle", active: retouchMode == "window") {
                        retouchMode = retouchMode == "window" ? "off" : "window"
                        if retouchMode == "window" { palette = .windows }
                    }
                    ToolChip(label: "Grad", icon: "plus.rectangle", active: retouchMode == "grad") {
                        retouchMode = retouchMode == "grad" ? "off" : "grad"
                    }
                }
            }) {
                if recipe.windows.isEmpty {
                    Text("Add a circle or gradient window, then tap or drag on the image. " +
                         "Select a window row, then drag on the image to move it.")
                        .font(.system(size: 10)).foregroundStyle(Kou.text3)
                        .fixedSize(horizontal: false, vertical: true)
                }
                ForEach(recipe.windows) { w in
                    if let i = recipe.windows.firstIndex(where: { $0.id == w.id }) {
                        WindowRow(w: $recipe.windows[i],
                                  selected: selWindow == w.id,
                                  onSelect: { selWindow = w.id }) {
                            if selWindow == w.id { selWindow = nil }
                            recipe.windows.remove(at: i)
                        }
                    }
                }
            }
        }
    }

    private var mixerPalette: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("RGB Mixer") {
                VStack(spacing: 5) {
                    ForEach(0..<3, id: \.self) { row in
                        HStack(spacing: 5) {
                            Text(["R′", "G′", "B′"][row])
                                .font(.system(size: 10, weight: .semibold))
                                .foregroundStyle([Color.red.opacity(0.9), .green.opacity(0.9), .blue.opacity(0.9)][row])
                                .frame(width: 14, alignment: .leading)
                            MixRow($recipe.mixer, row: row)
                        }
                    }
                }
                HStack {
                    Text("Monochrome").font(.system(size: 10.5)).foregroundStyle(Kou.text2)
                    Spacer()
                    Toggle("", isOn: Binding(
                        get: { recipe.mono != [0, 0, 0] },
                        set: { recipe.mono = $0 ? [0.21, 0.72, 0.07] : [0, 0, 0] }
                    ))
                    .labelsHidden().controlSize(.mini).tint(Kou.accent)
                }
                if recipe.mono != [0, 0, 0] {
                    TriRow("Mono", $recipe.mono, 0...1)
                }
            }
        }
    }

    private var retouchPalette: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("Retouch") {
                SegPicker([("off", "Off"), ("heal", "Heal"), ("clone", "Clone"),
                           ("dodge", "Dodge"), ("burn", "Burn")],
                          selection: $retouchMode)
                if retouchMode == "heal" {
                    SliderRow("Size", $spotSize, 0.01...0.15, reset: 0.05)
                } else if retouchMode == "clone" {
                    SliderRow("Radius", $cloneRadius, 0.02...0.2, reset: 0.06)
                    if pendingClone != nil {
                        Text("Tap destination")
                            .font(.system(size: 10, weight: .medium))
                            .foregroundStyle(Kou.accent)
                    }
                } else if retouchMode == "dodge" || retouchMode == "burn" {
                    SliderRow("Radius", $lightRadius, 0.05...0.6, reset: 0.25)
                    SliderRow("EV", $lightEV, 0...2, reset: 0.5)
                }
                if !recipe.spots.isEmpty || !recipe.lights.isEmpty || !recipe.clones.isEmpty {
                    ForEach(recipe.spots.indices, id: \.self) { i in
                        MarkRow("Spot \(i + 1)", icon: "bandage") { recipe.spots.remove(at: i) }
                    }
                    ForEach(recipe.clones.indices, id: \.self) { i in
                        MarkRow("Clone \(i + 1)", icon: "point.topleft.down.to.point.bottomright.curvepath") {
                            recipe.clones.remove(at: i)
                        }
                    }
                    ForEach(recipe.lights.indices, id: \.self) { i in
                        MarkRow("Light \(i + 1)  \(recipe.lights[i][3] >= 0 ? "+" : "")\(String(format: "%.1f", recipe.lights[i][3]))EV",
                                icon: "sun.max") {
                            recipe.lights.remove(at: i)
                        }
                    }
                }
            }
        }
    }

    private var detailPalette: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("Detail", trailing: {
                ToolChip(label: "Auto", icon: "wand.and.stars") { runAuto(.detail) }
                    .help("Estimate noise in flat areas and fringing at edges")
                    .opacity(autoBusy ? 0.5 : 1)
            }) {
                SliderRow("Sharpen", $recipe.sharpen, 0...1)
                SliderRow("Noise", $recipe.noise_luma, 0...1)
                SliderRow("NR Chroma", $recipe.noise_chroma, 0...1)
                SliderRow("Deband", $recipe.deband, 0...1)
                SliderRow("CA Fix", $recipe.ca_fix, 0...1)
                SliderRow("Beauty", $recipe.beauty, 0...1)
            }
        }
    }

    private var fxPalette: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("Effects", trailing: {
                ToolChip(label: "Place", icon: "plus", active: retouchMode == "flare") {
                    retouchMode = retouchMode == "flare" ? "off" : "flare"
                }
            }) {
                SliderRow("Clarity", $recipe.clarity, -1...1)
                SliderRow("Vignette", $recipe.vignette, -1...1)
                SliderRow("Grain", $recipe.grain, 0...1)
                SliderRow("Glow", $recipe.glow, 0...1)
                SliderRow("Flare", $recipe.flare[2], 0...1)
                SliderRow("Fl Hue", $recipe.flare[3], 0...1, track: Kou.hueTrack)
            }
        }
    }

    private var xformPalette: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("Transform", trailing: {
                ToolChip(label: "Auto", icon: "wand.and.stars") { runAuto(.geometry) }
                    .help("Detect horizon tilt and converging verticals")
                    .opacity(autoBusy ? 0.5 : 1)
            }) {
                SliderRow("Straighten", $recipe.rotation_deg, -10...10, step: 0.1)
                SliderRow("Keystone V", $recipe.key_v, -0.4...0.4)
                SliderRow("Keystone H", $recipe.key_h, -0.4...0.4)
            }
            Panel("Crop") {
                HStack(spacing: 4) {
                    Text("Aspect")
                        .font(.system(size: 10.5, weight: .medium))
                        .foregroundStyle(Kou.text2)
                        .frame(width: 60, alignment: .leading)
                    ForEach([(name: "Free", k: 0.0), ("1:1", 1.0), ("4:3", 4.0 / 3.0),
                             ("3:2", 1.5), ("16:9", 16.0 / 9.0)], id: \.name) { a in
                        ToolChip(label: a.name) { applyAspect(a.k) }
                    }
                }
                SliderRow("Left", $recipe.crop[0], 0...0.45)
                SliderRow("Top", $recipe.crop[1], 0...0.45)
                SliderRow("Right", $recipe.crop[2], 0...0.45)
                SliderRow("Bottom", $recipe.crop[3], 0...0.45)
            }
        }
    }

    /// DaVinci grade versions / stills: named snapshots of the current recipe,
    /// persisted inside the sidecar file on Save.
    private var versionsPalette: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("Versions", trailing: {
                ToolChip(label: "Save", icon: "plus") {
                    versionName = ""
                    showVersionName = true
                }
            }) {
                if versions.isEmpty {
                    Text("Save a named snapshot of the current grade, then click it " +
                         "to jump back. Versions persist inside the .koubou.json sidecar.")
                        .font(.system(size: 10)).foregroundStyle(Kou.text3)
                        .fixedSize(horizontal: false, vertical: true)
                }
                ForEach(versions) { v in
                    VersionRow(v: v, active: v.recipe == recipe,
                               onApply: { recipe = v.recipe },
                               onDelete: { versions.removeAll { $0.id == v.id } })
                }
            }
            // darktable history stack: every undo step as a clickable row.
            Panel("Edit History") {
                if undoStack.isEmpty {
                    Text("Edits appear here as you make them — click a step to jump back.")
                        .font(.system(size: 10)).foregroundStyle(Kou.text3)
                        .fixedSize(horizontal: false, vertical: true)
                }
                ForEach(Array(undoStack.enumerated().reversed()), id: \.offset) { i, _ in
                    Button {
                        jumpToHistory(i)
                    } label: {
                        HStack(spacing: 7) {
                            Text(i == 0 ? "0" : "\(i)")
                                .font(.system(size: 9).monospacedDigit())
                                .foregroundStyle(Kou.text3)
                                .frame(width: 14, alignment: .trailing)
                            Text(i == 0 ? "Original" : "Edit \(i)")
                                .font(.system(size: 10.5))
                                .foregroundStyle(Kou.text2)
                            Spacer()
                        }
                        .padding(.horizontal, 8).padding(.vertical, 4)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                }
                // current state marker — always the active row
                HStack(spacing: 7) {
                    Text("\(undoStack.count)")
                        .font(.system(size: 9).monospacedDigit())
                        .foregroundStyle(Kou.accent)
                        .frame(width: 14, alignment: .trailing)
                    Text("Current")
                        .font(.system(size: 10.5, weight: .semibold))
                        .foregroundStyle(Kou.text1)
                    Spacer()
                }
                .padding(.horizontal, 8).padding(.vertical, 4)
            }
        }
    }

    private var actionBar: some View {
        VStack(spacing: 0) {
            Kou.hairline.frame(height: 1)
            HStack(spacing: 6) {
                if rendering {
                    ProgressView().controlSize(.small).tint(Kou.accent)
                        .frame(width: 14, height: 14)
                }
                Text(status.isEmpty ? (dirty ? "Unsaved changes" : photo.name) : status)
                    .font(.system(size: 10).monospacedDigit())
                    .foregroundStyle(dirty && status.isEmpty ? Kou.accent : Kou.text3)
                    .lineLimit(1).truncationMode(.middle)
                    .help(status)
                Spacer()
                IconAction(icon: "doc.on.doc") { copyRecipe() }
                IconAction(icon: "clipboard") { pasteRecipe() }
                IconAction(icon: "arrow.counterclockwise") { recipe = Recipe() }
                IconAction(icon: "square.stack.3d.down.forward") { store.editAsDocument() }
                    .help("Edit in layered document")
                Button("Save") { save() }.buttonStyle(KouSecondaryButton())
                Button("Export") { export() }.buttonStyle(KouPrimaryButton())
            }
            .padding(.horizontal, 10).padding(.vertical, 8)
            .background(Kou.bg1)
        }
    }

    // MARK: data plumbing (unchanged logic)

    /// .cube LUT file open panel; stores the path (parser rejects junk).
    private func pickLut() {
        let p = NSOpenPanel()
        p.canChooseFiles = true
        p.canChooseDirectories = false
        p.allowsMultipleSelection = false
        if let cube = UniformTypeIdentifiers.UTType(filenameExtension: "cube") {
            p.allowedContentTypes = [cube]
        }
        if p.runModal() == .OK, let u = p.url {
            recipe.lut_file = u.path
            if recipe.lut_amount == 0 { recipe.lut_amount = 1 }
        }
    }

    /// which suggestion subset a per-panel Auto chip applies
    private enum AutoScope { case all, tone, geometry, detail, zones, colour }

    /// engine-side analysis on a neutral preview, then apply the scope's
    /// suggestions (core/src/auto.rs). One analysis per click.
    private func runAuto(_ scope: AutoScope) {
        guard !autoBusy else { return }
        autoBusy = true
        status = "Analyzing…"
        let p = photo.path
        Task {
            let sug = await KouEngine.shared.work { $0.autoAnalyze(path: p) }
            await MainActor.run {
                autoBusy = false
                guard let s = sug else {
                    status = "Auto analysis failed: \(KouEngine.shared.lastError)"
                    return
                }
                switch scope {
                case .all:
                    recipe.wb_mode = .auto
                    recipe.auto_exposure = true
                    recipe.auto_contrast = true
                    recipe.rotation_deg = s.rotation_deg
                    recipe.key_v = s.key_v; recipe.key_h = s.key_h
                    recipe.noise_luma = s.noise_luma; recipe.noise_chroma = s.noise_chroma
                    recipe.ca_fix = s.ca_fix
                    recipe.vibrance = s.vibrance
                    recipe.zones_ev = s.zones_ev
                case .tone:
                    recipe.wb_mode = .auto
                    recipe.auto_exposure = true
                    recipe.auto_contrast = true
                case .geometry:
                    recipe.rotation_deg = s.rotation_deg
                    recipe.key_v = s.key_v; recipe.key_h = s.key_h
                case .detail:
                    recipe.noise_luma = s.noise_luma; recipe.noise_chroma = s.noise_chroma
                    recipe.ca_fix = s.ca_fix
                case .zones:
                    recipe.zones_ev = s.zones_ev
                case .colour:
                    recipe.vibrance = s.vibrance
                }
                status = autoSummary(s, scope)
            }
        }
    }

    /// status line describing what THIS chip actually applied (not the full
    /// suggestion, which may include fields other scopes own)
    private func autoSummary(_ s: AutoSuggestion, _ scope: AutoScope) -> String {
        var parts: [String] = []
        switch scope {
        case .all:
            return "Auto: \(s.summary)"
        case .tone:
            return "Auto: WB + exposure + contrast"
        case .geometry:
            if s.rotation_deg != 0 { parts.append(String(format: "straighten %+.1f°", s.rotation_deg)) }
            if s.key_v != 0 { parts.append(String(format: "keystone %+.2f", s.key_v)) }
        case .detail:
            if s.noise_luma > 0 { parts.append(String(format: "NR %.2f (σ=%.1f)", s.noise_luma, s.noise_sigma)) }
            if s.ca_fix > 0 { parts.append(String(format: "CA %.2f", s.ca_fix)) }
        case .zones:
            if s.zones_ev.contains(where: { $0 != 0 }) { parts.append("zone EQ set") }
        case .colour:
            if s.vibrance != 0 { parts.append(String(format: "vibrance %+.2f", s.vibrance)) }
        }
        return parts.isEmpty ? "Auto: nothing to correct" : "Auto: \(parts.joined(separator: " · "))"
    }

    private func applyAspect(_ k: Double) {
        if k == 0 { recipe.crop = [0, 0, 0, 0]; return }
        guard let img = image else { return }
        let remW = 1 - recipe.crop[0] - recipe.crop[2]
        let remH = 1 - recipe.crop[1] - recipe.crop[3]
        guard remW > 0.02, remH > 0.02 else { return }
        // frame aspect ≈ dst aspect corrected back for the current crop
        let fa = (Double(img.width) / Double(img.height)) * remH / remW
        let cx = recipe.crop[0] + remW / 2
        let cy = recipe.crop[1] + remH / 2
        var w = remW
        var h = w * fa / k
        if h > remH { h = remH; w = h * k / fa }
        w = min(w, 1); h = min(h, 1)
        let l = (cx - w / 2).clamped(to: 0...1)
        let t = (cy - h / 2).clamped(to: 0...1)
        recipe.crop = [l, t,
                       (1 - (l + w)).clamped(to: 0...1),
                       (1 - (t + h)).clamped(to: 0...1)]
    }

    private func curveBinding(_ ch: Int) -> Binding<[[Double]]> {
        switch ch {
        case 0: return $recipe.curve
        case 1: return $recipe.curve_r
        case 2: return $recipe.curve_g
        case 3: return $recipe.curve_b
        case 4: return $recipe.hue_hue
        case 5: return $recipe.hue_sat
        case 6: return $recipe.hue_lum
        case 7: return $recipe.lum_sat
        default: return $recipe.sat_sat
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

    private func load() {
        let sc = KouEngine.shared.sidecar(path: photo.path)
        baseline = sc.recipe
        baselineVersions = sc.versions
        versions = store.unsavedVersions[photo.path] ?? sc.versions
        recipe = store.unsavedEdits[photo.path] ?? sc.recipe
        rating = sc.rating
        label = sc.label
        loadedRating = sc.rating
        loadedLabel = sc.label
        image = nil
        baselineImg = nil
        undoStack.removeAll()
        redoStack.removeAll()
        cmp = .off
        zoom = 1
        pan = .zero
        rerender()
    }

    private func save() {
        var sc = Sidecar()
        sc.rating = rating
        sc.label = label
        sc.recipe = recipe
        sc.versions = versions
        let stem = URL(fileURLWithPath: photo.path).deletingPathExtension().lastPathComponent
        let ok = KouEngine.shared.writeSidecar(path: photo.path, sc)
        status = ok ? "Saved \(stem).koubou.json"
                    : "Save failed: \(KouEngine.shared.lastError)"
        if ok {
            baseline = recipe
            baselineVersions = versions
            baselineImg = nil   // re-render compare base with the saved recipe
            store.unsavedEdits.removeValue(forKey: photo.path)
            store.unsavedVersions.removeValue(forKey: photo.path)
            dirty = false
        }
    }

    // MARK: undo / redo

    /// Called from the recipe onChange with the pre-edit value. Edits within
    /// 0.8s of the previous change coalesce into one undo step (slider drags).
    private func recordUndo(_ old: Recipe) {
        if applyingHistory { return }
        let now = Date()
        if now.timeIntervalSince(lastEditTime) > 0.8 {
            undoStack.append(old)
            if undoStack.count > 80 { undoStack.removeFirst() }
        }
        lastEditTime = now
        redoStack.removeAll()
    }

    private func undo() {
        guard let prev = undoStack.popLast() else { status = "Nothing to undo"; return }
        applyingHistory = true   // cleared by the next recipe onChange
        redoStack.append(recipe)
        recipe = prev
        status = "Undo"
    }

    private func redo() {
        guard let next = redoStack.popLast() else { status = "Nothing to redo"; return }
        applyingHistory = true
        undoStack.append(recipe)
        recipe = next
        status = "Redo"
    }

    // MARK: compare baseline render

    /// Render the saved-state image once for wipe/before/diff modes.
    private func ensureBaseline() {
        if baselineImg != nil { return }
        let r = baseline
        Task.detached { [path = photo.path] in
            let img = await KouEngine.shared.work { $0.render(path: path, recipe: r, maxPx: 1400).0 }
            await MainActor.run { baselineImg = img }
        }
    }

    // MARK: qualifier picking

    /// Read a 5×5 mean pixel from the displayed render at dst-normalized coords.
    private func samplePixel(nx: Double, ny: Double) -> (r: Double, g: Double, b: Double)? {
        guard let image, let provider = image.dataProvider,
              let cf = provider.data else { return nil }
        let data = cf as Data
        let w = image.width, h = image.height
        let px = min(max(Int(nx * Double(w)), 0), w - 1)
        let py = min(max(Int(ny * Double(h)), 0), h - 1)
        var r = 0.0, g = 0.0, b = 0.0, n = 0
        for dy in -2...2 {
            for dx in -2...2 {
                let x = px + dx, y = py + dy
                guard x >= 0, x < w, y >= 0, y < h else { continue }
                let o = (y * w + x) * 4
                if o + 2 < data.count {
                    r += Double(data[o]); g += Double(data[o + 1]); b += Double(data[o + 2]); n += 1
                }
            }
        }
        guard n > 0 else { return nil }
        return (r / Double(n) / 255, g / Double(n) / 255, b / Double(n) / 255)
    }

    private func rgbHsv(_ c: (Double, Double, Double)) -> (h: Double, s: Double, v: Double) {
        let (r, g, b) = c
        let mx = max(r, g, b), mn = min(r, g, b), d = mx - mn
        var h = 0.0
        if d > 1e-6 {
            if mx == r { h = ((g - b) / d).truncatingRemainder(dividingBy: 6) }
            else if mx == g { h = (b - r) / d + 2 }
            else { h = (r - g) / d + 4 }
            h /= 6
            if h < 0 { h += 1 }
        }
        return (h, mx > 1e-6 ? d / mx : 0, mx)
    }

    private func hueDist(_ a: Double, _ b: Double) -> Double {
        let d = abs(a - b)
        return min(d, 1 - d)
    }

    /// Apply an eyedropper sample to the qualifier ranges.
    private func qualifierPick(_ c: (Double, Double, Double), mode: String) {
        let (hh, ss, _) = rgbHsv(c)
        let l = 0.2126 * c.0 + 0.7152 * c.1 + 0.0722 * c.2
        switch mode {
        case "qpick":
            recipe.qh = [hh, 0.08, 0.08]
            recipe.qs = [max(0, ss - 0.18), min(1, ss + 0.18), 0.12]
            recipe.ql = [max(0, l - 0.28), min(1, l + 0.28), 0.18]
            recipe.q_enabled = true
            status = String(format: "Keyed h=%.2f s=%.2f l=%.2f", hh, ss, l)
        case "qadd":
            recipe.qh[1] = max(recipe.qh[1], hueDist(hh, recipe.qh[0]) + 0.03)
            recipe.qs[0] = min(recipe.qs[0], ss)
            recipe.qs[1] = max(recipe.qs[1], ss)
            recipe.ql[0] = min(recipe.ql[0], l)
            recipe.ql[1] = max(recipe.ql[1], l)
        case "qsub":
            if hueDist(hh, recipe.qh[0]) < recipe.qh[1] {
                recipe.qh[1] = max(0.004, hueDist(hh, recipe.qh[0]) - 0.02)
            }
            if ss > recipe.qs[0] && ss < recipe.qs[1] {
                if ss - recipe.qs[0] < recipe.qs[1] - ss {
                    recipe.qs[0] = min(1, ss + 0.02)
                } else {
                    recipe.qs[1] = max(0, ss - 0.02)
                }
            }
            if l > recipe.ql[0] && l < recipe.ql[1] {
                if l - recipe.ql[0] < recipe.ql[1] - l {
                    recipe.ql[0] = min(1, l + 0.02)
                } else {
                    recipe.ql[1] = max(0, l - 0.02)
                }
            }
        default: break
        }
        recipe.q_enabled = true
    }

    // MARK: palette bookkeeping

    /// Amber-dot indicator: does this palette hold non-default values?
    private func paletteDirty(_ p: Palette) -> Bool {
        let d = Recipe()
        switch p {
        case .quick:
            return recipe.exposure != d.exposure || recipe.contrast != d.contrast
                || recipe.highlights != d.highlights || recipe.shadows != d.shadows
                || recipe.whites != d.whites || recipe.blacks != d.blacks
                || recipe.temperature != d.temperature || recipe.tint != d.tint
                || recipe.wb_mode != d.wb_mode || recipe.wb_pick != d.wb_pick
                || recipe.saturation != d.saturation || recipe.vibrance != d.vibrance
                || recipe.clarity != d.clarity || recipe.rotation_deg != d.rotation_deg
                || recipe.auto_exposure || recipe.auto_contrast
        case .light:
            return recipe.exposure != d.exposure || recipe.contrast != d.contrast
                || recipe.highlights != d.highlights || recipe.shadows != d.shadows
                || recipe.whites != d.whites || recipe.blacks != d.blacks
                || recipe.temperature != d.temperature || recipe.tint != d.tint
                || recipe.wb_mode != d.wb_mode || recipe.wb_pick != d.wb_pick
                || recipe.saturation != d.saturation || recipe.vibrance != d.vibrance
                || recipe.auto_exposure || recipe.auto_contrast
                || recipe.pivot != d.pivot
                || recipe.highlight_rolloff != d.highlight_rolloff
                || recipe.shadow_rolloff != d.shadow_rolloff
        case .wheels:
            return recipe.lift != d.lift || recipe.gamma != d.gamma
                || recipe.gain != d.gain || recipe.offset != d.offset
                || recipe.shadow_sat != d.shadow_sat || recipe.midtone_sat != d.midtone_sat
                || recipe.highlight_sat != d.highlight_sat
                || recipe.shadow_hue != d.shadow_hue || recipe.midtone_hue != d.midtone_hue
                || recipe.highlight_hue != d.highlight_hue || !recipe.look.isEmpty
        case .curves:
            return !recipe.curve.isEmpty || !recipe.curve_r.isEmpty
                || !recipe.curve_g.isEmpty || !recipe.curve_b.isEmpty
                || !recipe.hue_hue.isEmpty || !recipe.hue_sat.isEmpty
                || !recipe.hue_lum.isEmpty || !recipe.lum_sat.isEmpty
                || !recipe.sat_sat.isEmpty
        case .zones:
            return recipe.z_dark != d.z_dark || recipe.z_shadow != d.z_shadow
                || recipe.z_light != d.z_light || recipe.z_global != d.z_global
                || recipe.zones_ev != d.zones_ev
        case .qualifier:
            return recipe.q_enabled || recipe.q_show
        case .windows:
            return !recipe.windows.isEmpty
        case .mixer:
            return recipe.mixer != d.mixer || recipe.mono != d.mono
        case .retouch:
            return !recipe.spots.isEmpty || !recipe.lights.isEmpty || !recipe.clones.isEmpty
        case .detail:
            return recipe.sharpen != 0 || recipe.noise_luma != 0 || recipe.noise_chroma != 0
                || recipe.deband != 0 || recipe.ca_fix != 0 || recipe.beauty != 0
        case .fx:
            return recipe.clarity != 0 || recipe.vignette != 0 || recipe.grain != 0
                || recipe.glow != 0 || recipe.flare[2] != 0
        case .xform:
            return recipe.rotation_deg != 0 || recipe.crop != d.crop
                || recipe.key_v != 0 || recipe.key_h != 0
        case .meters, .versions:
            return false
        }
    }

    /// Reset every field owned by a palette (DaVinci palette reset).
    private func resetPalette(_ p: Palette) {
        let d = Recipe()
        switch p {
        case .quick:
            recipe.exposure = d.exposure; recipe.contrast = d.contrast
            recipe.highlights = d.highlights; recipe.shadows = d.shadows
            recipe.whites = d.whites; recipe.blacks = d.blacks
            recipe.temperature = d.temperature; recipe.tint = d.tint
            recipe.wb_mode = d.wb_mode; recipe.wb_pick = d.wb_pick
            recipe.saturation = d.saturation; recipe.vibrance = d.vibrance
            recipe.clarity = d.clarity; recipe.rotation_deg = d.rotation_deg
            recipe.auto_exposure = false; recipe.auto_contrast = false
        case .light:
            recipe.exposure = d.exposure; recipe.contrast = d.contrast
            recipe.highlights = d.highlights; recipe.shadows = d.shadows
            recipe.whites = d.whites; recipe.blacks = d.blacks
            recipe.temperature = d.temperature; recipe.tint = d.tint
            recipe.wb_mode = d.wb_mode; recipe.wb_pick = d.wb_pick
            recipe.saturation = d.saturation; recipe.vibrance = d.vibrance
            recipe.auto_exposure = false; recipe.auto_contrast = false
            recipe.pivot = d.pivot
            recipe.highlight_rolloff = d.highlight_rolloff
            recipe.shadow_rolloff = d.shadow_rolloff
        case .wheels:
            recipe.lift = d.lift; recipe.gamma = d.gamma; recipe.gain = d.gain
            recipe.offset = d.offset
            recipe.shadow_hue = d.shadow_hue; recipe.shadow_sat = d.shadow_sat
            recipe.midtone_hue = d.midtone_hue; recipe.midtone_sat = d.midtone_sat
            recipe.highlight_hue = d.highlight_hue; recipe.highlight_sat = d.highlight_sat
            recipe.look = ""
        case .curves:
            recipe.curve = []; recipe.curve_r = []; recipe.curve_g = []; recipe.curve_b = []
            recipe.hue_hue = []; recipe.hue_sat = []; recipe.hue_lum = []
            recipe.lum_sat = []; recipe.sat_sat = []
        case .zones:
            recipe.z_dark = d.z_dark; recipe.z_shadow = d.z_shadow
            recipe.z_light = d.z_light; recipe.z_global = d.z_global
            recipe.zones_ev = d.zones_ev
            zoneImgMode = false
        case .qualifier:
            recipe.qh = d.qh; recipe.qs = d.qs; recipe.ql = d.ql; recipe.qadj = d.qadj
            recipe.q_invert = false; recipe.q_clean = d.q_clean; recipe.q_blur = 0
            recipe.q_show = false; recipe.q_enabled = false
        case .windows:
            recipe.windows = []
            selWindow = nil
        case .mixer:
            recipe.mixer = d.mixer; recipe.mono = d.mono
        case .retouch:
            recipe.spots = []; recipe.lights = []; recipe.clones = []
            pendingClone = nil
        case .detail:
            recipe.sharpen = 0; recipe.noise_luma = 0; recipe.noise_chroma = 0
            recipe.deband = 0; recipe.ca_fix = 0; recipe.beauty = 0
        case .fx:
            recipe.clarity = 0; recipe.vignette = 0; recipe.grain = 0
            recipe.glow = 0; recipe.flare = d.flare
        case .xform:
            recipe.rotation_deg = 0; recipe.crop = d.crop
            recipe.key_v = 0; recipe.key_h = 0
        case .meters, .versions:
            break
        }
        status = "Reset \(p.title)"
    }

    // MARK: navigation / ratings / versions

    private func stepPhoto(_ dir: Int) {
        // match by path — rating/label edits mutate Photo values and would
        // break a Hashable-equality lookup
        guard let i = store.filtered.firstIndex(where: { $0.path == photo.path }) else { return }
        let j = i + dir
        guard store.filtered.indices.contains(j) else { return }
        store.selection = store.filtered[j]
    }

    private func setRating(_ r: Int) {
        rating = (rating == r) ? 0 : r
        if rating != loadedRating {
            loadedRating = rating
            store.setRating(path: photo.path, rating)
        }
        status = "Rating \(rating)"
    }

    /// DaVinci "Apply Grade from One Clip Prior" (Cmd+=): copy the previous
    /// photo's recipe (its unsaved edits win over its sidecar).
    private func applyPrevRecipe() {
        guard let i = store.filtered.firstIndex(where: { $0.path == photo.path }), i > 0 else {
            status = "No previous photo"
            return
        }
        let prev = store.filtered[i - 1]
        let r = store.unsavedEdits[prev.path] ?? KouEngine.shared.sidecar(path: prev.path).recipe
        recipe = r
        status = "Applied grade from \(prev.name)"
    }

    private func commitVersion() {
        let name = versionName.trimmingCharacters(in: .whitespaces)
        versions.append(GradeVersion(
            name: name.isEmpty ? "Version \(versions.count + 1)" : name,
            recipe: recipe))
        versionName = ""
        dirty = true
    }

    /// apply a saved version — goes through undo history like any edit
    private func applyVersion(_ v: GradeVersion) {
        recipe = v.recipe
        status = "Applied \(v.name)"
    }

    /// darktable history jump: restore the recipe snapshot at undo index i.
    private func jumpToHistory(_ i: Int) {
        guard undoStack.indices.contains(i) else { return }
        applyingHistory = true
        let target = undoStack[i]
        undoStack.removeSubrange(i...)
        redoStack.removeAll()
        recipe = target
        status = i == 0 ? "Back to Original" : "Back to edit \(i)"
    }

    private func copyRecipe() {
        guard let data = try? JSONEncoder().encode(recipe),
              let js = String(data: data, encoding: .utf8) else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(js, forType: .string)
        status = "Recipe copied"
    }

    private func pasteRecipe() {
        guard let js = NSPasteboard.general.string(forType: .string),
              let data = js.data(using: .utf8),
              let r = try? JSONDecoder().decode(Recipe.self, from: data) else {
            status = "No recipe on clipboard"
            return
        }
        recipe = r
        status = "Recipe pasted"
    }

    /// Downsampled sRGB copy of the current render for the cursor probe
    /// and the zone-EQ hover readout (160px wide, ~55KB).
    private func makeProbe(_ img: CGImage) {
        let pw = 160
        let ph = max(1, Int(160.0 * Double(img.height) / Double(img.width)))
        var buf = [UInt8](repeating: 0, count: pw * ph * 4)
        buf.withUnsafeMutableBytes { ptr in
            guard let ctx = CGContext(data: ptr.baseAddress, width: pw, height: ph,
                                      bitsPerComponent: 8, bytesPerRow: pw * 4,
                                      space: CGColorSpaceCreateDeviceRGB(),
                                      bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
            else { return }
            ctx.interpolationQuality = .medium
            ctx.draw(img, in: CGRect(x: 0, y: 0, width: pw, height: ph))
        }
        probeBuf = buf
        probeW = pw
        probeH = ph
    }

    private func scheduleRender() {
        renderTask?.cancel()
        renderTask = Task {
            try? await Task.sleep(nanoseconds: 250_000_000)
            guard !Task.isCancelled else { return }
            rerender()
        }
    }

    private func rerender() {
        rendering = true
        renderGen += 1
        let gen = renderGen
        let r = recipe
        Task.detached { [path = photo.path] in
            let (img, bins, wv, vc, ce) = await KouEngine.shared.work {
                $0.renderScopes(path: path, recipe: r, maxPx: 1400)
            }
            await MainActor.run {
                // drop stale results — a newer render already started
                guard gen == renderGen else { return }
                if let img { image = img; makeProbe(img) }
                hist = (0..<4).map { ch in Array(bins[(ch * 256)..<(ch * 256 + 256)]) }
                wave = wv
                vec = vc
                cie = ce
                rendering = false
            }
        }
    }

    private func export() {
        let panel = NSSavePanel()
        panel.allowedContentTypes = [.jpeg]
        panel.nameFieldStringValue = photo.name.replacingOccurrences(
            of: "." + (photo.name.split(separator: ".").last.map(String.init) ?? ""),
            with: ".jpg")
        guard panel.runModal() == .OK, let url = panel.url else { return }
        rendering = true
        status = "Exporting…"
        let r = recipe
        Task.detached { [path = photo.path] in
            let img = await KouEngine.shared.work { $0.export(path: path, recipe: r) }
            await MainActor.run {
                rendering = false
                guard let img,
                      let dest = CGImageDestinationCreateWithURL(url as CFURL, UTType.jpeg.identifier as CFString, 1, nil)
                else {
                    status = "Export failed: \(KouEngine.shared.lastError)"
                    return
                }
                CGImageDestinationAddImage(dest, img, [kCGImageDestinationLossyCompressionQuality: 0.92] as CFDictionary)
                status = CGImageDestinationFinalize(dest) ? "Exported \(url.lastPathComponent)" : "Export failed"
            }
        }
    }
}

/// horizontal filmstrip thumb used at the bottom of the stage
struct FilmCell: View {
    let photo: Photo
    let selected: Bool
    var dirty: Bool = false
    @State private var image: CGImage?

    var body: some View {
        ZStack(alignment: .topLeading) {
            Kou.bg3
            if let image {
                Image(image, scale: 1, label: Text(photo.name))
                    .resizable().scaledToFill()
            }
            if let c = labelColors.first(where: { $0.name == photo.label })?.color {
                Circle().fill(c).frame(width: 6, height: 6).padding(4)
            }
            if dirty {
                VStack {
                    Spacer()
                    HStack {
                        Spacer()
                        Circle()
                            .fill(Kou.accent)
                            .frame(width: 7, height: 7)
                            .overlay(Circle().stroke(.black.opacity(0.6), lineWidth: 0.75))
                            .padding(4)
                    }
                }
            }
        }
        .frame(width: 84, height: 56)
        .clipShape(RoundedRectangle(cornerRadius: 5))
        .overlay(RoundedRectangle(cornerRadius: 5)
            .stroke(selected ? Kou.accent : Kou.hairline, lineWidth: selected ? 2 : 1))
        .opacity(selected ? 1 : 0.75)
        .task {
            let img = await KouEngine.shared.work { $0.thumbnail(path: photo.path, maxPx: 160) }
            await MainActor.run { image = img }
        }
    }
}

/// Look preset chooser with live thumbnails: the popover renders each look
/// over the current recipe at thumbnail size, cached per photo (DaVinci LUT
/// gallery / darktable preset style).
struct LookPicker: View {
    @Binding var look: String
    let photo: Photo
    let recipe: Recipe

    @State private var open = false
    @State private var thumbs: [String: CGImage] = [:]
    @State private var thumbPath = ""
    @State private var loading = false

    private let options: [(String, String)] = [
        ("", "None"), ("teal_orange", "Teal & Orange"), ("film_fade", "Film Fade"),
        ("bleach", "Bleach Bypass"), ("noir", "Noir"), ("matte", "Matte"),
    ]

    var body: some View {
        Button {
            if thumbPath != photo.path { thumbs = [:]; thumbPath = photo.path }
            open.toggle()
            if open { loadThumbs() }
        } label: {
            HStack(spacing: 4) {
                Text(options.first(where: { $0.0 == look })?.1 ?? "Look")
                    .font(.system(size: 9.5, weight: .medium))
                Image(systemName: "chevron.up.chevron.down")
                    .font(.system(size: 7))
            }
            .foregroundStyle(look.isEmpty ? Kou.text2 : Kou.accent)
            .padding(.horizontal, 8).padding(.vertical, 3)
            .background(Capsule().fill(look.isEmpty ? Kou.bg3 : Kou.accentSoft)
                .overlay(Capsule().stroke(look.isEmpty ? Kou.hairline : Kou.accent.opacity(0.4), lineWidth: 0.5)))
        }
        .buttonStyle(.plain)
        .popover(isPresented: $open, arrowEdge: .bottom) {
            VStack(alignment: .leading, spacing: 8) {
                Text("LOOKS")
                    .font(.system(size: 9, weight: .semibold)).tracking(1.2)
                    .foregroundStyle(Kou.text3)
                LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 6), count: 3),
                          spacing: 8) {
                    ForEach(options, id: \.0) { v, name in
                        VStack(spacing: 3) {
                            ZStack {
                                Kou.bg3
                                if let img = thumbs[v] {
                                    Image(img, scale: 1, label: Text(name))
                                        .resizable().scaledToFill()
                                } else {
                                    ProgressView().controlSize(.mini).tint(Kou.text3)
                                }
                            }
                            .aspectRatio(1.5, contentMode: .fit)
                            .clipped()
                            Text(name)
                                .font(.system(size: 8, weight: v == look ? .semibold : .regular))
                                .foregroundStyle(v == look ? Kou.accent : Kou.text3)
                                .lineLimit(1)
                        }
                        .padding(4)
                        .background(Kou.bg2)
                        .clipShape(RoundedRectangle(cornerRadius: 6))
                        .overlay(RoundedRectangle(cornerRadius: 6)
                            .stroke(v == look ? Kou.accent : Kou.hairline,
                                    lineWidth: v == look ? 1.5 : 0.5))
                        .contentShape(Rectangle())
                        .onTapGesture { look = v; open = false }
                    }
                }
            }
            .padding(10)
            .frame(width: 300)
            .background(Kou.bg1)
        }
    }

    /// One render per look at 220px; engine cache makes this fast enough.
    private func loadThumbs() {
        if loading { return }
        loading = true
        let path = photo.path
        let base = recipe
        Task.detached {
            var out: [String: CGImage] = [:]
            for (v, _) in options {
                var r = base
                r.look = v
                let (img, _) = await KouEngine.shared.work {
                    $0.render(path: path, recipe: r, maxPx: 220)
                }
                if let img { out[v] = img }
            }
            await MainActor.run {
                thumbs.merge(out) { _, n in n }
                loading = false
            }
        }
    }
}

/// DaVinci-style color wheel: drag sets chroma offset; the luma slider under
/// the wheel sets the mean component. Double-click resets chroma to neutral.
struct ColorWheel: View {
    let title: String
    @Binding var v: [Double]
    /// neutral mean: 0 for additive wheels, 1 for multiplicative
    let center: Double

    private var mean: Double { (v[0] + v[1] + v[2]) / 3 }
    private var chroma: (u: Double, w: Double) {
        let m = mean
        let d = (v[0] - m, v[1] - m, v[2] - m)
        return (d.0 - (d.1 + d.2) / 2, (d.1 - d.2) * 0.8660)
    }

    var body: some View {
        VStack(spacing: 3) {
            GeometryReader { geo in
                let s = min(geo.size.width, geo.size.height)
                let cx = geo.size.width / 2, cy = geo.size.height / 2
                let r = s / 2 - 3
                ZStack {
                    Circle().fill(
                        AngularGradient(colors: [
                            .red, .yellow, .green, .cyan, .blue,
                            Color(red: 1, green: 0, blue: 1), .red,
                        ], center: .center)
                    )
                    .opacity(0.55)
                    Circle().fill(
                        RadialGradient(colors: [Kou.bg2.opacity(0.85), .clear],
                                       center: .center, startRadius: 0, endRadius: r * 0.75)
                    )
                    Circle().stroke(Kou.border, lineWidth: 0.5)
                    // crosshair
                    Path { p in
                        p.move(to: .init(x: cx - r, y: cy)); p.addLine(to: .init(x: cx + r, y: cy))
                        p.move(to: .init(x: cx, y: cy - r)); p.addLine(to: .init(x: cx, y: cy + r))
                    }
                    .stroke(Kou.text3.opacity(0.4), lineWidth: 0.5)
                    let c = chroma
                    let bx = cx + CGFloat(c.u) * r * 4
                    let by = cy - CGFloat(c.w) * r * 4
                    Circle()
                        .fill(.white)
                        .frame(width: 9, height: 9)
                        .overlay(Circle().stroke(.black.opacity(0.65), lineWidth: 1))
                        .shadow(color: .black.opacity(0.6), radius: 1.5)
                        .position(x: bx.clamped(to: cx - r...cx + r),
                                  y: by.clamped(to: cy - r...cy + r))
                }
                .frame(width: s, height: s)
                .position(x: cx, y: cy)
                .gesture(DragGesture(minimumDistance: 0).onChanged { g in
                    let dx = Double(g.location.x - cx) / Double(r)
                    let dy = Double(cy - g.location.y) / Double(r)
                    let m = min((dx * dx + dy * dy).squareRoot(), 1.0) / 4
                    let a = atan2(dy, dx)
                    let h = a / (2 * .pi)
                    let rgb = hueRgb(h)
                    let cm = (rgb.0 + rgb.1 + rgb.2) / 3
                    v = [
                        mean + m * (rgb.0 - cm) * 1.7,
                        mean + m * (rgb.1 - cm) * 1.7,
                        mean + m * (rgb.2 - cm) * 1.7,
                    ]
                })
                .onTapGesture(count: 2) { _ in
                    v = [mean, mean, mean]
                }
            }
            .aspectRatio(1, contentMode: .fit)
            TrackSlider(value: Binding(
                get: { mean },
                set: { m in
                    let d = (v[0] - mean, v[1] - mean, v[2] - mean)
                    v = [m + d.0, m + d.1, m + d.2]
                }),
                range: center == 0 ? -0.25...0.25 : 0.4...1.6,
                step: 0.01, reset: center, height: 12)
            Text(title.uppercased())
                .font(.system(size: 8, weight: .semibold)).tracking(1)
                .foregroundStyle(Kou.text3)
            Text(String(format: "%.2f  %.2f  %.2f", v[0], v[1], v[2]))
                .font(.system(size: 7.5).monospacedDigit())
                .foregroundStyle(
                    v == [center, center, center] ? Kou.text3 : Kou.accent.opacity(0.8))
                .lineLimit(1).minimumScaleFactor(0.8)
        }
    }
}

private func hueRgb(_ h: Double) -> (Double, Double, Double) {
    let h6 = (h - h.rounded(.down)) * 6
    let i = Int(h6) % 6
    let f = h6 - h6.rounded(.down)
    let seg = [(1.0, 0.0, 0.0), (1.0, 1.0, 0.0), (0.0, 1.0, 0.0),
               (0.0, 1.0, 1.0), (0.0, 0.0, 1.0), (1.0, 0.0, 1.0)]
    let a = seg[i], b = seg[(i + 1) % 6]
    return (a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f, a.2 + (b.2 - a.2) * f)
}

/// HDR/log zone wheel row: hue, amount, exposure, sat for one tonal band.
struct ZoneRow: View {
    let title: String
    @Binding var z: [Double]
    init(_ t: String, _ z: Binding<[Double]>) {
        title = t
        _z = z
    }
    var body: some View {
        VStack(spacing: 4) {
            HStack(spacing: 7) {
                Text(title.uppercased())
                    .font(.system(size: 9.5, weight: .semibold)).tracking(0.8)
                    .foregroundStyle(Kou.text2)
                    .frame(width: 52, alignment: .leading)
                TrackSlider(value: $z[2], range: -1...1)
                Text(String(format: "%+.2f", z[2]))
                    .font(.system(size: 10).monospacedDigit())
                    .foregroundStyle(z[2] == 0 ? Kou.text3 : Kou.accent)
                    .frame(width: 38, alignment: .trailing)
            }
            HStack(spacing: 5) {
                Text("hue")
                    .font(.system(size: 8.5)).foregroundStyle(Kou.text3)
                    .frame(width: 52, alignment: .leading)
                TrackSlider(value: $z[0], range: 0...1, height: 12, track: Kou.hueTrack)
                TrackSlider(value: $z[1], range: 0...1, height: 12)
                TrackSlider(value: $z[3], range: -1...1, height: 12)
            }
        }
    }
}

/// curve editor: click to add, drag to move, double-click a point to delete.
/// `hist` (luma histogram, 256 bins) is drawn as a faint backdrop like DaVinci.
struct CurveEditor: View {
    @Binding var points: [[Double]]
    var tint: Color = .white
    var hist: [UInt32] = []
    /// darktable colour equalizer: rainbow spectrum along the input axis for
    /// the hue-vs-* curves
    var spectrum: Bool = false

    var body: some View {
        GeometryReader { geo in
            let w = geo.size.width, h = geo.size.height
            ZStack {
                if spectrum {
                    LinearGradient(colors: [
                        .red, .orange, .yellow, .green, .cyan, .blue,
                        Color(hue: 0.83, saturation: 0.85, brightness: 0.9), .red,
                    ], startPoint: .leading, endPoint: .trailing)
                    .opacity(0.20)
                } else {
                    Rectangle().fill(Color(red: 0.05, green: 0.05, blue: 0.065))
                }
                Canvas { ctx, size in
                    // luma histogram backdrop
                    if hist.count == 256 {
                        let maxv = Double(hist.max() ?? 1)
                        var hp = Path()
                        hp.move(to: .init(x: 0, y: size.height))
                        for x in 0..<256 {
                            let v = min(log1p(Double(hist[x])) / log1p(maxv + 1), 1)
                            hp.addLine(to: .init(x: CGFloat(x) / 255 * size.width,
                                                 y: size.height - v * size.height))
                        }
                        hp.addLine(to: .init(x: size.width, y: size.height))
                        hp.closeSubpath()
                        ctx.fill(hp, with: .color(.white.opacity(0.07)))
                    }
                    // grid
                    for i in 1..<4 {
                        let f = CGFloat(i) / 4
                        var gp = Path()
                        gp.move(to: .init(x: f * size.width, y: 0))
                        gp.addLine(to: .init(x: f * size.width, y: size.height))
                        gp.move(to: .init(x: 0, y: f * size.height))
                        gp.addLine(to: .init(x: size.width, y: f * size.height))
                        ctx.stroke(gp, with: .color(.white.opacity(0.07)), lineWidth: 0.5)
                    }
                    // diagonal reference (dashed)
                    var dp = Path()
                    dp.move(to: .init(x: 0, y: size.height))
                    dp.addLine(to: .init(x: size.width, y: 0))
                    ctx.stroke(dp, with: .color(.white.opacity(0.12)),
                               style: StrokeStyle(lineWidth: 0.5, dash: [3, 3]))
                    let pts = sorted()
                    if pts.count > 1 {
                        var p = Path()
                        var fill = Path()
                        for i in 0...64 {
                            let x = Double(i) / 64
                            let y = eval(pts, x)
                            let px = x * size.width
                            let py = (1 - y) * size.height
                            if i == 0 {
                                p.move(to: .init(x: px, y: py))
                                fill.move(to: .init(x: px, y: py))
                            } else {
                                p.addLine(to: .init(x: px, y: py))
                                fill.addLine(to: .init(x: px, y: py))
                            }
                        }
                        fill.addLine(to: .init(x: size.width, y: size.height))
                        fill.addLine(to: .init(x: 0, y: size.height))
                        fill.closeSubpath()
                        ctx.fill(fill, with: .color(tint.opacity(0.10)))
                        ctx.stroke(p, with: .color(tint), lineWidth: 1.5)
                    }
                    for pt in pts {
                        let px = pt[0] * size.width
                        let py = (1 - pt[1]) * size.height
                        let dot = Circle().path(in: CGRect(x: px - 4, y: py - 4, width: 8, height: 8))
                        ctx.fill(dot, with: .color(.white))
                        let ring = Circle().path(in: CGRect(x: px - 4.5, y: py - 4.5, width: 9, height: 9))
                        ctx.stroke(ring, with: .color(tint), lineWidth: 1.5)
                    }
                }
            }
            .clipShape(RoundedRectangle(cornerRadius: 5))
            .overlay(RoundedRectangle(cornerRadius: 5).stroke(Kou.hairline, lineWidth: 1))
            .contentShape(Rectangle())
            .gesture(DragGesture(minimumDistance: 0)
                .onChanged { g in
                    let x = Double(g.location.x / w).clamped(to: 0...1)
                    let y = Double(1 - g.location.y / h).clamped(to: 0...1)
                    if g.translation == .zero, points.isEmpty == false,
                       let i = nearest(x, y), dist(points[i], x, y) < 0.06 {
                        points[i] = [x, y]
                        return
                    }
                    if let i = dragging(points, g.startLocation, w, h) {
                        points[i] = [x, y]
                    }
                }
                .onEnded { g in
                    let x = Double(g.location.x / w).clamped(to: 0...1)
                    let y = Double(1 - g.location.y / h).clamped(to: 0...1)
                    if nearest(x, y) == nil || dist(points[nearest(x, y)!], x, y) > 0.06 {
                        points.append([x, y])
                    }
                })
            .onTapGesture(count: 2) { loc in
                let x = Double(loc.x / w), y = Double(1 - loc.y / h)
                if let i = nearest(x, y), dist(points[i], x, y) < 0.08, points.count > 0 {
                    points.remove(at: i)
                }
            }
        }
    }

    private func sorted() -> [[Double]] {
        points.sorted { $0[0] < $1[0] }
    }
    private func nearest(_ x: Double, _ y: Double) -> Int? {
        guard !points.isEmpty else { return nil }
        var bi = 0, bd = Double.greatestFiniteMagnitude
        for (i, p) in points.enumerated() {
            let d = dist(p, x, y)
            if d < bd { bd = d; bi = i }
        }
        return bi
    }
    private func dist(_ p: [Double], _ x: Double, _ y: Double) -> Double {
        (p[0] - x) * (p[0] - x) + (p[1] - y) * (p[1] - y)
    }
    private func dragging(_ pts: [[Double]], _ start: CGPoint, _ w: CGFloat, _ h: CGFloat) -> Int? {
        let x = Double(start.x / w), y = Double(1 - start.y / h)
        return nearest(x, y)
    }
    /// catmull-rom evaluation matching the engine's curve_lut
    private func eval(_ pts: [[Double]], _ x: Double) -> Double {
        if pts.isEmpty { return x }
        if x <= pts.first![0] { return pts.first![1] }
        if x >= pts.last![0] { return pts.last![1] }
        var i = 0
        while i + 1 < pts.count && pts[i + 1][0] < x { i += 1 }
        let p0 = pts[max(i - 1, 0)], p1 = pts[i], p2 = pts[min(i + 1, pts.count - 1)],
            p3 = pts[min(i + 2, pts.count - 1)]
        let t = (x - p1[0]) / max(p2[0] - p1[0], 1e-6)
        let t2 = t * t, t3 = t2 * t
        return 0.5 * (2 * p1[1] + (-p0[1] + p2[1]) * t
            + (2 * p0[1] - 5 * p1[1] + 4 * p2[1] - p3[1]) * t2
            + (-p0[1] + 3 * p1[1] - 3 * p2[1] + p3[1]) * t3)
    }
}

/// scopes: parade waveform / vectorscope / CIE xy rendered as image planes
struct ScopesView: View {
    let wave: [UInt32]
    let vec: [UInt32]
    let cie: [UInt32]
    let kind: String

    /// approx CIE 1931 spectral locus (x, y) from violet through green to red,
    /// closed by the purple line — normalised 0..1 coords
    private let locus: [(Double, Double)] = [
        (0.1741, 0.0050), (0.1733, 0.0048), (0.1703, 0.0058), (0.1566, 0.0177),
        (0.1440, 0.0297), (0.1247, 0.0578), (0.0913, 0.1327), (0.0454, 0.2950),
        (0.0082, 0.5384), (0.0039, 0.6548), (0.0139, 0.7502), (0.0743, 0.8338),
        (0.1147, 0.8262), (0.1547, 0.8059), (0.1929, 0.7816), (0.2296, 0.7543),
        (0.2658, 0.7243), (0.3016, 0.6923), (0.3373, 0.6589), (0.3731, 0.6245),
        (0.4087, 0.5896), (0.4441, 0.5547), (0.4788, 0.5202), (0.5125, 0.4866),
        (0.5448, 0.4544), (0.5752, 0.4242), (0.6029, 0.3965), (0.6270, 0.3725),
        (0.6482, 0.3514), (0.6658, 0.3340), (0.6801, 0.3197), (0.6915, 0.3083),
        (0.7006, 0.2993), (0.7079, 0.2920), (0.7132, 0.2868), (0.7173, 0.2827),
        (0.7190, 0.2809), (0.7206, 0.2794), (0.7260, 0.2740), (0.7300, 0.2700),
        (0.7311, 0.2689), (0.7320, 0.2680), (0.7327, 0.2673), (0.7334, 0.2666),
        (0.7340, 0.2660), (0.7344, 0.2656), (0.7347, 0.2653),
    ]

    var body: some View {
        Canvas { ctx, size in
            switch kind {
            case "parade":
                // channel separators + level guides
                for c in 0..<2 {
                    var sep = Path()
                    let x = size.width * CGFloat(c + 1) / 3
                    sep.move(to: .init(x: x, y: 0)); sep.addLine(to: .init(x: x, y: size.height))
                    ctx.stroke(sep, with: .color(.white.opacity(0.10)), lineWidth: 0.5)
                }
                for f in [0.25, 0.5, 0.75] {
                    var gl = Path()
                    let y = size.height * CGFloat(f)
                    gl.move(to: .init(x: 0, y: y)); gl.addLine(to: .init(x: size.width, y: y))
                    ctx.stroke(gl, with: .color(.white.opacity(0.08)), lineWidth: 0.5)
                }
                if wave.count >= 196608 {
                    let cols: [Color] = [.red, .green, .blue]
                    for c in 0..<3 {
                        let maxv = Double(wave[(c * 65536)..<(c * 65536 + 65536)].max() ?? 1).squareRoot()
                        for v in 0..<256 {
                            for x in 0..<256 {
                                let n = wave[c * 65536 + v * 256 + x]
                                if n == 0 { continue }
                                let a = min(Double(n).squareRoot() / maxv, 1)
                                let px = (CGFloat(c * 256 + x) / 768) * size.width
                                let py = size.height - CGFloat(v) / 256 * size.height
                                ctx.fill(
                                    Path(CGRect(x: px, y: py, width: size.width / 256 + 0.6, height: size.height / 256 + 0.6)),
                                    with: .color(cols[c].opacity(a)))
                            }
                        }
                    }
                }
            case "vector":
                let c = CGPoint(x: size.width / 2, y: size.height / 2)
                // graticule: outer + inner circles, cross axes, skin-tone line
                for f in [0.7, 0.35] {
                    let d = size.width * f
                    ctx.stroke(Circle().path(in: CGRect(
                        x: c.x - d / 2, y: c.y - d / 2, width: d, height: d)),
                        with: .color(.white.opacity(0.12)), lineWidth: 0.5)
                }
                var ax = Path()
                ax.move(to: .init(x: c.x - size.width * 0.35, y: c.y))
                ax.addLine(to: .init(x: c.x + size.width * 0.35, y: c.y))
                ax.move(to: .init(x: c.x, y: c.y - size.height * 0.35))
                ax.addLine(to: .init(x: c.x, y: c.y + size.height * 0.35))
                ctx.stroke(ax, with: .color(.white.opacity(0.10)), lineWidth: 0.5)
                // skin tone line: Cr+, Cb- → upper-left
                var sk = Path()
                sk.move(to: c)
                sk.addLine(to: .init(x: c.x - size.width * 0.16, y: c.y - size.height * 0.30))
                ctx.stroke(sk, with: .color(.orange.opacity(0.6)),
                           style: StrokeStyle(lineWidth: 1, dash: [3, 2]))
                if vec.count >= 65536 {
                    let maxv = Double(vec.max() ?? 1).squareRoot()
                    for y in 0..<256 {
                        for x in 0..<256 {
                            let n = vec[y * 256 + x]
                            if n == 0 { continue }
                            let a = min(Double(n).squareRoot() / maxv, 1)
                            let px = CGFloat(x) / 256 * size.width
                            let py = CGFloat(255 - y) / 256 * size.height
                            ctx.fill(
                                Path(CGRect(x: px, y: py, width: size.width / 256 + 0.5, height: size.height / 256 + 0.5)),
                                with: .color(.cyan.opacity(a)))
                        }
                    }
                }
            default:
                // CIE xy: spectral locus outline + D65 marker
                var lp = Path()
                for (i, p) in locus.enumerated() {
                    let px = p.0 * size.width
                    let py = (1 - p.1) * size.height
                    if i == 0 { lp.move(to: .init(x: px, y: py)) }
                    else { lp.addLine(to: .init(x: px, y: py)) }
                }
                lp.closeSubpath()
                ctx.stroke(lp, with: .color(.white.opacity(0.35)), lineWidth: 0.75)
                if cie.count >= 65536 {
                    let maxv = Double(cie.max() ?? 1).squareRoot()
                    for y in 0..<256 {
                        for x in 0..<256 {
                            let n = cie[y * 256 + x]
                            if n == 0 { continue }
                            let a = min(Double(n).squareRoot() / maxv, 1)
                            let px = CGFloat(x) / 256 * size.width
                            let py = size.height - CGFloat(y) / 256 * size.height
                            ctx.fill(
                                Path(CGRect(x: px, y: py, width: size.width / 256 + 0.5, height: size.height / 256 + 0.5)),
                                with: .color(.green.opacity(a)))
                        }
                    }
                }
                // D65 white point
                let d65 = CGRect(x: 0.3127 * size.width - 3, y: (1 - 0.3290) * size.height - 3,
                                 width: 6, height: 6)
                ctx.stroke(Circle().path(in: d65), with: .color(.white.opacity(0.6)), lineWidth: 0.75)
            }
        }
        .background(Color(red: 0.045, green: 0.045, blue: 0.055))
        .clipShape(RoundedRectangle(cornerRadius: 5))
        .overlay(RoundedRectangle(cornerRadius: 5).stroke(Kou.hairline, lineWidth: 1))
    }
}

/// R,G,B + luma overlay histogram (log scale, filled areas).
struct HistogramView: View {
    let hist: [[UInt32]]

    var body: some View {
        Canvas { ctx, size in
            // 25% guides
            for f in [0.25, 0.5, 0.75] {
                var gl = Path()
                let y = size.height * CGFloat(f)
                gl.move(to: .init(x: 0, y: y)); gl.addLine(to: .init(x: size.width, y: y))
                ctx.stroke(gl, with: .color(.white.opacity(0.07)), lineWidth: 0.5)
            }
            let chans: [(Int, Color)] = [
                (0, .red), (1, .green), (2, .blue), (3, .white),
            ]
            let maxv = hist.flatMap { $0 }.map { log1p(Double($0)) }.max() ?? 1
            for (ch, color) in chans where hist.count > ch {
                var p = Path()
                var fill = Path()
                for x in 0..<256 {
                    let v = log1p(Double(hist[ch][x])) / maxv
                    let px = CGFloat(x) / 255 * size.width
                    let py = size.height - CGFloat(v) * size.height
                    if x == 0 {
                        p.move(to: CGPoint(x: px, y: py))
                        fill.move(to: CGPoint(x: px, y: py))
                    } else {
                        p.addLine(to: CGPoint(x: px, y: py))
                        fill.addLine(to: CGPoint(x: px, y: py))
                    }
                }
                fill.addLine(to: .init(x: size.width, y: size.height))
                fill.addLine(to: .init(x: 0, y: size.height))
                fill.closeSubpath()
                ctx.fill(fill, with: .color(color.opacity(ch == 3 ? 0.22 : 0.18)))
                ctx.stroke(p, with: .color(color.opacity(ch == 3 ? 0.8 : 0.6)), lineWidth: 1)
            }
        }
        .background(Color(red: 0.05, green: 0.05, blue: 0.06))
        .clipShape(RoundedRectangle(cornerRadius: 5))
        .overlay(RoundedRectangle(cornerRadius: 5).stroke(Kou.hairline, lineWidth: 1))
    }
}

struct LabelPicker: View {
    @Binding var label: String

    var body: some View {
        // padded hit area per dot — same fix as Stars (bare 11px dots
        // silently missed taps landing in the inter-dot gaps)
        HStack(spacing: 2) {
            ForEach(labelColors, id: \.name) { l in
                Circle()
                    .fill(l.color)
                    .frame(width: 11, height: 11)
                    .overlay(Circle().stroke(.white.opacity(0.9), lineWidth: label == l.name ? 1.5 : 0))
                    .opacity(label.isEmpty || label == l.name ? 1 : 0.4)
                    .frame(width: 15, height: 16)
                    .contentShape(Rectangle())
                    .onTapGesture { label = (label == l.name) ? "" : l.name }
            }
            if !label.isEmpty {
                Button { label = "" } label: {
                    Image(systemName: "xmark.circle.fill")
                        .font(.system(size: 10))
                        .foregroundStyle(Kou.text3)
                }
                .buttonStyle(.plain)
            }
        }
    }
}

extension EditorView {
    /// marker overlay for heal spots / dodge-burn lights / clones / windows / flare
    @ViewBuilder
    func retouchMarkers(in rect: CGRect) -> some View {
        let cl = recipe.crop[0], ct = recipe.crop[1]
        let sw = (1 - recipe.crop[0] - recipe.crop[2])
        let sh = (1 - recipe.crop[1] - recipe.crop[3])
        Canvas { ctx, _ in
        let fx2sx = { (fx: Double) -> CGFloat in rect.minX + CGFloat((fx - cl) / max(sw, 0.01)) * rect.width }
        let fy2sy = { (fy: Double) -> CGFloat in rect.minY + CGFloat((fy - ct) / max(sh, 0.01)) * rect.height }
        for s in recipe.spots {
            let cx = fx2sx(s[0])
            let cy = fy2sy(s[1])
            let r = s[2] * rect.width / max(sw, 0.01)
            let path = Circle().path(in: CGRect(x: cx - r, y: cy - r, width: r * 2, height: r * 2))
            ctx.stroke(path, with: .color(.red.opacity(0.9)), lineWidth: 1.5)
        }
        for c in recipe.clones {
            let sx = fx2sx(c[0]), sy = fy2sy(c[1])
            let dx = fx2sx(c[2]), dy = fy2sy(c[3])
            let r = c[4] * rect.width / max(sw, 0.01)
            ctx.stroke(Circle().path(in: CGRect(x: sx - r, y: sy - r, width: r * 2, height: r * 2)),
                       with: .color(.green.opacity(0.9)), lineWidth: 1.5)
            ctx.stroke(Circle().path(in: CGRect(x: dx - r, y: dy - r, width: r * 2, height: r * 2)),
                       with: .color(.orange.opacity(0.9)), lineWidth: 1.5)
            var ln = Path()
            ln.move(to: .init(x: sx, y: sy))
            ln.addLine(to: .init(x: dx, y: dy))
            ctx.stroke(ln, with: .color(.white.opacity(0.5)), lineWidth: 0.8)
        }
        for l in recipe.lights {
            let cx = rect.minX + l[0] * rect.width
            let cy = rect.minY + l[1] * rect.height
            let r = l[2] * rect.width
            let path = Circle().path(in: CGRect(x: cx - r, y: cy - r, width: r * 2, height: r * 2))
            let col: Color = l[3] >= 0 ? .yellow : .purple
            ctx.stroke(path, with: .color(col.opacity(0.9)), lineWidth: 1.5)
        }
        for w in recipe.windows {
            // DaVinci overlay: white-ish outline; the selected window is amber
            // and thicker; a disabled window is dimmed to a hairline.
            let sel = w.id == selWindow
            let col: Color = sel ? Kou.accent : .cyan
            let alpha: Double = w.enabled ? (sel ? 1.0 : 0.85) : 0.25
            let lw: CGFloat = sel ? 2.5 : 1.5
            if w.kind == "gradient" {
                var ln = Path()
                ln.move(to: .init(x: rect.minX + w.p[0] * rect.width, y: rect.minY + w.p[1] * rect.height))
                ln.addLine(to: .init(x: rect.minX + w.p[2] * rect.width, y: rect.minY + w.p[3] * rect.height))
                ctx.stroke(ln, with: .color(col.opacity(alpha)), lineWidth: lw)
                // direction tick: short perpendicular at midpoint
                let mx = (w.p[0] + w.p[2]) / 2, my = (w.p[1] + w.p[3]) / 2
                let dx = w.p[2] - w.p[0], dy = w.p[3] - w.p[1]
                let len = max((dx * dx + dy * dy).squareRoot(), 1e-4)
                let nx2 = -dy / len, ny2 = dx / len
                var tick = Path()
                tick.move(to: .init(x: rect.minX + mx * rect.width, y: rect.minY + my * rect.height))
                tick.addLine(to: .init(x: rect.minX + (mx + nx2 * 0.04) * rect.width,
                                       y: rect.minY + (my + ny2 * 0.04) * rect.height))
                ctx.stroke(tick, with: .color(col.opacity(alpha)), lineWidth: lw)
            } else {
                let cx = fx2sx(w.p[0])
                let cy = fy2sy(w.p[1])
                let rx = w.p[2] * rect.width / max(sw, 0.01)
                let ry = w.p[3] * rect.height / max(sh, 0.01)
                ctx.stroke(Ellipse().path(in: CGRect(x: cx - rx, y: cy - ry, width: rx * 2, height: ry * 2)),
                           with: .color(col.opacity(alpha)), lineWidth: lw)
                if sel {
                    // centre cross for the selected window
                    var cr = Path()
                    cr.move(to: .init(x: cx - 6, y: cy)); cr.addLine(to: .init(x: cx + 6, y: cy))
                    cr.move(to: .init(x: cx, y: cy - 6)); cr.addLine(to: .init(x: cx, y: cy + 6))
                    ctx.stroke(cr, with: .color(Kou.accent.opacity(0.9)), lineWidth: 1)
                }
            }
        }
        if recipe.wb_mode == .pick {
            let cx = fx2sx(recipe.wb_pick[0])
            let cy = fy2sy(recipe.wb_pick[1])
            var cross = Path()
            cross.move(to: .init(x: cx - 8, y: cy)); cross.addLine(to: .init(x: cx + 8, y: cy))
            cross.move(to: .init(x: cx, y: cy - 8)); cross.addLine(to: .init(x: cx, y: cy + 8))
            ctx.stroke(cross, with: .color(.white), lineWidth: 1.5)
        }
        if recipe.flare[2] > 0 {
            let cx = rect.minX + recipe.flare[0] * rect.width
            let cy = rect.minY + recipe.flare[1] * rect.height
            var cross = Path()
            cross.move(to: .init(x: cx - 10, y: cy)); cross.addLine(to: .init(x: cx + 10, y: cy))
            cross.move(to: .init(x: cx, y: cy - 10)); cross.addLine(to: .init(x: cx, y: cy + 10))
            ctx.stroke(cross, with: .color(.yellow), lineWidth: 1.5)
        }
        }
    }
}

/// delete row for a placed spot/light
struct MarkRow: View {
    let title: String
    let icon: String
    let onDelete: () -> Void
    init(_ title: String, icon: String = "circle.fill", onDelete: @escaping () -> Void) {
        self.title = title
        self.icon = icon
        self.onDelete = onDelete
    }
    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: icon).font(.system(size: 9)).foregroundStyle(Kou.text3)
            Text(title).font(.system(size: 10.5)).foregroundStyle(Kou.text1)
            Spacer()
            Button { onDelete() } label: {
                Image(systemName: "xmark")
                    .font(.system(size: 9, weight: .bold))
                    .foregroundStyle(Kou.text3)
            }
            .buttonStyle(.plain)
        }
        .padding(.horizontal, 8).padding(.vertical, 4)
        .background(RoundedRectangle(cornerRadius: 5).fill(Kou.bg3))
    }
}

/// three-channel row (R G B) bound to a [Double] of length 3
struct TriRow: View {
    let title: String
    @Binding var v: [Double]
    let range: ClosedRange<Double>
    init(_ title: String, _ v: Binding<[Double]>, _ range: ClosedRange<Double>) {
        self.title = title
        self._v = v
        self.range = range
    }
    var body: some View {
        if v.count == 3 {
            HStack(spacing: 5) {
                Text(title)
                    .font(.system(size: 10.5)).foregroundStyle(Kou.text2)
                    .frame(width: 34, alignment: .leading)
                ForEach(0..<3, id: \.self) { i in
                    TrackSlider(value: $v[i], range: range, reset: 0.5)
                }
            }
        }
    }
}

/// one row of the RGB mixer matrix
struct MixRow: View {
    @Binding var m: [Double]
    let row: Int
    init(_ m: Binding<[Double]>, row: Int) {
        _m = m
        self.row = row
    }
    var body: some View {
        if m.count == 9 {
            HStack(spacing: 5) {
                ForEach(0..<3, id: \.self) { c in
                    TrackSlider(value: $m[row * 3 + c], range: -1...2,
                                reset: c == row ? 1.0 : 0.0)
                }
            }
        }
    }
}

/// per-window editor rows: visibility eye (DaVinci power-window on/off),
/// invert, per-window opacity, geometry sliders. Selecting a row arms it
/// for dragging on the stage.
struct WindowRow: View {
    @Binding var w: PowerWindow
    var selected = false
    var onSelect: () -> Void = {}
    let onDelete: () -> Void
    var body: some View {
        VStack(spacing: 5) {
            HStack {
                Image(systemName: w.kind == "gradient" ? "rectangle.lefthalf.filled" : "circle")
                    .font(.system(size: 9))
                    .foregroundStyle(selected ? Kou.accent : .cyan)
                Text(w.kind == "gradient" ? "Gradient" : "Circle")
                    .font(.system(size: 10.5, weight: .medium)).foregroundStyle(Kou.text1)
                Spacer()
                // on/off eye (DaVinci per-window visibility)
                Button { w.enabled.toggle() } label: {
                    Image(systemName: w.enabled ? "eye" : "eye.slash")
                        .font(.system(size: 9.5))
                        .foregroundStyle(w.enabled ? Kou.text2 : Kou.text3)
                }
                .buttonStyle(.plain)
                .help("Window on/off")
                Text("Inv").font(.system(size: 9.5)).foregroundStyle(Kou.text2)
                Toggle("", isOn: $w.invert).labelsHidden().controlSize(.mini).tint(Kou.accent)
                Button { onDelete() } label: {
                    Image(systemName: "xmark").font(.system(size: 9, weight: .bold))
                        .foregroundStyle(Kou.text3)
                }
                .buttonStyle(.plain)
            }
            SliderRow("EV", $w.ev, -2...2)
            SliderRow("Sat", $w.sat, -1...1)
            SliderRow("Temp", $w.temp, -1...1)
            SliderRow("Opacity", $w.opacity, 0...1, reset: 1)
            if w.kind == "circle" {
                SliderRow("Size", $w.p[2], 0.02...0.6, reset: 0.15)
                SliderRow("Ratio", $w.p[3], 0.02...0.6, reset: 0.15)
                SliderRow("Rot", $w.p[4], -90...90)
            }
            SliderRow("Soft", $w.p[5], 0.02...1, reset: 0.4)
        }
        .padding(8)
        .background(selected ? Kou.accentSoft.opacity(0.5) : Kou.bg3)
        .clipShape(RoundedRectangle(cornerRadius: 6))
        .overlay(RoundedRectangle(cornerRadius: 6)
            .stroke(selected ? Kou.accent.opacity(0.6) : Kou.hairline, lineWidth: 1))
        .contentShape(Rectangle())
        .onTapGesture { onSelect() }
        .opacity(w.enabled ? 1 : 0.65)
    }
}

/// DaVinci "stills"/versions row: click to apply that snapshot.
struct VersionRow: View {
    let v: GradeVersion
    var active = false
    var onApply: () -> Void = {}
    var onDelete: () -> Void = {}
    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "photo.stack")
                .font(.system(size: 9))
                .foregroundStyle(active ? Kou.accent : Kou.text3)
            Text(v.name.isEmpty ? "Version" : v.name)
                .font(.system(size: 10.5, weight: active ? .semibold : .regular))
                .foregroundStyle(active ? Kou.accent : Kou.text1)
                .lineLimit(1)
            Spacer()
            Button { onDelete() } label: {
                Image(systemName: "xmark")
                    .font(.system(size: 9, weight: .bold))
                    .foregroundStyle(Kou.text3)
            }
            .buttonStyle(.plain)
        }
        .padding(.horizontal, 8).padding(.vertical, 6)
        .background(RoundedRectangle(cornerRadius: 5)
            .fill(active ? Kou.accentSoft : Kou.bg3))
        .contentShape(Rectangle())
        .onTapGesture { onApply() }
    }
}

/// Installs a local NSEvent monitor for bare-key shortcuts (digits, arrows,
/// B, W, Esc). Handler returns true when the key was consumed.
final class KeyMonitor {
    private var monitor: Any?
    var handler: (NSEvent) -> Bool = { _ in false }
    func install() {
        guard monitor == nil else { return }
        monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] ev in
            guard let self else { return ev }
            return self.handler(ev) ? nil : ev
        }
    }
    func uninstall() {
        if let m = monitor { NSEvent.removeMonitor(m); monitor = nil }
    }
}

/// Scroll-wheel zoom on the stage (hover-gated in the handler).
/// Handler returns true to let the event pass through untouched.
final class ScrollMonitor {
    private var monitor: Any?
    var handler: (NSEvent) -> Bool = { _ in true }
    func install() {
        guard monitor == nil else { return }
        monitor = NSEvent.addLocalMonitorForEvents(matching: .scrollWheel) { [weak self] ev in
            guard let self else { return ev }
            return self.handler(ev) ? ev : nil
        }
    }
    func uninstall() {
        if let m = monitor { NSEvent.removeMonitor(m); monitor = nil }
    }
}

struct Stars: View {
    @Binding var rating: Int
    var body: some View {
        // generous hit area per star — the bare 12pt glyph is ~4px wide and
        // swallowed most taps, silently leaving `rating` unchanged
        HStack(spacing: 0) {
            ForEach(1...5, id: \.self) { i in
                Image(systemName: i <= rating ? "star.fill" : "star")
                    .font(.system(size: 12))
                    .foregroundStyle(i <= rating ? Kou.gold : Kou.text3)
                    .frame(width: 15, height: 16)
                    .contentShape(Rectangle())
                    .onTapGesture { rating = (rating == i) ? 0 : i }
            }
        }
    }
}
