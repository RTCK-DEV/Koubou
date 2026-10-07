import SwiftUI
import AppKit
import Speech
import UniformTypeIdentifiers

/// Motion workspace — a .kmotion timeline driven by `tl.*` commands on a
/// DocSession. Preview renders come from tl.renderFrame; export is ffmpeg
/// inside koubou-motion.
final class MotionStore: ObservableObject {
    @Published var opened = false
    @Published var busy = false
    @Published var error: String?

    @Published var name = "Untitled"
    @Published var w = 1920
    @Published var h = 1080
    @Published var fps = 24.0
    @Published var duration = 0.0
    @Published var tracks: [TlTrack] = []
    @Published var playhead = 0.0
    @Published var selectedClip: UInt64?
    @Published var preview: CGImage?
    @Published var dirty = false
    @Published var generating = false
    @Published var transcribing = false

    private var session: DocSession?

    struct TlTrack: Identifiable {
        let id: Int
        var kind: String
        var muted: Bool
        var clips: [TlClip]
        var cues: [[String: Any]]
    }
    struct TlClip: Identifiable {
        let id: UInt64
        var src: String
        var inPoint: Double
        var outPoint: Double
        var offset: Double
        var opacity: [[Double]]
        var scale: [[Double]]
        var fadeIn: Double
        var fadeOut: Double
        var dur: Double { max(0.01, outPoint - inPoint) }
        var label: String { src.isEmpty ? "clip" : URL(fileURLWithPath: src).deletingPathExtension().lastPathComponent }
    }

    private func ensure() -> DocSession? {
        if session == nil { session = DocSession() }
        return session
    }

    enum After { case none, reload, preview }

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
                    switch then {
                    case .none: break
                    case .reload: self.reloadState()
                    case .preview: self.renderFrame()
                    }
                } else {
                    self.error = r["error"] as? String ?? "command failed"
                }
            }
        }
    }

    func newTimeline() {
        opened = true
        dispatch(["id": "tl.new", "w": w, "h": h, "fps": fps, "name": name])
    }

    func open(_ url: URL) {
        opened = true
        dispatch(["id": "tl.open", "path": url.path])
    }

    func save() {
        let p = NSSavePanel()
        p.nameFieldStringValue = name + ".kmotion"
        guard p.runModal() == .OK, let url = p.url else { return }
        dispatch(["id": "tl.save", "path": url.path], then: .none)
        dirty = false
    }

    func reloadState() {
        guard let s = session else { return }
        Task.detached { [weak self] in
            let dj = await s.work { $0.dispatch(["id": "tl.json"])["result"] as? [String: Any] ?? [:] }
            await MainActor.run { self?.applyState(dj) }
        }
    }

    private func applyState(_ d: [String: Any]) {
        name = d["name"] as? String ?? name
        w = (d["w"] as? NSNumber)?.intValue ?? w
        h = (d["h"] as? NSNumber)?.intValue ?? h
        fps = (d["fps"] as? NSNumber)?.doubleValue ?? fps
        duration = (d["duration"] as? NSNumber)?.doubleValue ?? duration
        let trs = d["tracks"] as? [[String: Any]] ?? []
        tracks = trs.enumerated().map { (i, t) in
            let clips = (t["clips"] as? [[String: Any]] ?? []).map { c -> TlClip in
                TlClip(
                    id: (c["id"] as? NSNumber)?.uint64Value ?? 0,
                    src: c["src"] as? String ?? "",
                    inPoint: (c["in"] as? NSNumber)?.doubleValue ?? 0,
                    outPoint: (c["out"] as? NSNumber)?.doubleValue ?? 1,
                    offset: (c["offset"] as? NSNumber)?.doubleValue ?? 0,
                    opacity: (c["opacity"] as? [[NSNumber]] ?? []).map { $0.map { $0.doubleValue } },
                    scale: (c["scale"] as? [[NSNumber]] ?? []).map { $0.map { $0.doubleValue } },
                    fadeIn: (c["fadeIn"] as? NSNumber)?.doubleValue ?? 0,
                    fadeOut: (c["fadeOut"] as? NSNumber)?.doubleValue ?? 0)
            }
            return TlTrack(id: i, kind: t["kind"] as? String ?? "video",
                           muted: t["muted"] as? Bool ?? false,
                           clips: clips, cues: t["cues"] as? [[String: Any]] ?? [])
        }
        renderFrame()
    }

    func renderFrame() {
        guard let s = session else { return }
        let t = playhead
        Task.detached { [weak self] in
            let img = await s.work { sess in
                DocStore.decodePNG(sess.dispatch(["id": "tl.renderFrame", "t": t]))
            }
            await MainActor.run { self?.preview = img }
        }
    }

    // ---- editing ops ----

    func addTrack(kind: String) { dispatch(["id": "tl.addTrack", "kind": kind]) }

    func addClip() {
        let p = NSOpenPanel()
        p.allowedContentTypes = [.movie, .mpeg4Movie, .quickTimeMovie, .audio]
        p.allowsMultipleSelection = false
        guard p.runModal() == .OK, let url = p.url else { return }
        let vtrack = tracks.firstIndex(where: { $0.kind == "video" }) ?? {
            addTrack(kind: "video"); return 0 }()
        dispatch(["id": "tl.addClip", "track": vtrack, "src": url.path,
                  "in": 0.0, "out": 10.0, "offset": duration], then: .preview)
    }

    func addAudio() {
        let p = NSOpenPanel()
        p.allowedContentTypes = [.audio]
        guard p.runModal() == .OK, let url = p.url else { return }
        var atrack = tracks.firstIndex(where: { $0.kind == "audio" })
        if atrack == nil { addTrack(kind: "audio"); atrack = tracks.count }
        dispatch(["id": "tl.addClip", "track": atrack ?? 1, "src": url.path,
                  "in": 0.0, "out": 10.0, "offset": 0.0], then: .preview)
    }

    func setClip(_ id: UInt64, _ params: [String: Any]) {
        var cmd: [String: Any] = ["id": "tl.setClip", "clip": id]
        cmd.merge(params) { _, n in n }
        dispatch(cmd, then: .preview)
    }

    func removeClip() {
        guard let id = selectedClip else { return }
        dispatch(["id": "tl.removeClip", "clip": id])
        selectedClip = nil
    }

    func exportMovie() {
        let p = NSSavePanel()
        p.nameFieldStringValue = name + ".mp4"
        guard p.runModal() == .OK, let url = p.url else { return }
        dispatch(["id": "tl.render", "out": url.path], then: .none)
    }

    /// ffmpeg silencedetect on the selected clip → ranges shown for the user.
    @Published var silenceRanges: [(Double, Double)] = []
    func detectSilence() {
        guard let id = selectedClip,
              let clip = tracks.flatMap(\.clips).first(where: { $0.id == id }),
              !clip.src.isEmpty, let s = ensure() else { return }
        Task.detached { [weak self] in
            let r = await s.work {
                $0.dispatch(["id": "tl.detectSilence", "path": clip.src])
            }
            await MainActor.run {
                if let res = r["result"] as? [String: Any],
                   let ranges = res["ranges"] as? [[String: NSNumber]] {
                    self?.silenceRanges = ranges.map { ($0["start"]?.doubleValue ?? 0, $0["end"]?.doubleValue ?? 0) }
                }
            }
        }
    }

    /// Utsushi-style transcription: audio clip → subtitle cues via the
    /// platform Speech framework (permission-gated).
    func transcribe() {
        guard let id = selectedClip,
              let clip = tracks.flatMap(\.clips).first(where: { $0.id == id }),
              !clip.src.isEmpty, let s = ensure() else {
            error = "select an audio/video clip first"; return
        }
        transcribing = true
        SFSpeechRecognizer.requestAuthorization { auth in
            guard auth == .authorized else {
                DispatchQueue.main.async {
                    self.transcribing = false
                    self.error = "speech recognition not authorized"
                }
                return
            }
            let url = URL(fileURLWithPath: clip.src)
            let rec = SFSpeechRecognizer(locale: Locale(identifier: "ja-JP")) ?? SFSpeechRecognizer()
            let req = SFSpeechURLRecognitionRequest(url: url)
            rec?.recognitionTask(with: req) { result, err in
                if let result, result.isFinal {
                    let cues: [[String: Any]] = result.bestTranscription.segments.map {
                        ["t": clip.offset + $0.timestamp,
                         "dur": $0.duration,
                         "text": $0.substring]
                    }
                    DispatchQueue.main.async {
                        self.transcribing = false
                        if self.tracks.firstIndex(where: { $0.kind == "subtitle" }) == nil {
                            self.dispatch(["id": "tl.addTrack", "kind": "subtitle"], then: .none)
                        }
                        for c in cues {
                            self.dispatch(["id": "tl.addCue", "t": c["t"]!, "dur": c["dur"]!, "text": c["text"]!], then: .none)
                        }
                        self.reloadState()
                    }
                }
                if let err {
                    DispatchQueue.main.async {
                        self.transcribing = false
                        self.error = err.localizedDescription
                    }
                }
            }
        }
    }

    /// minimax-h3 integration: prompt → h3ui backend → generated mp4 as clip.
    func generateClip(endpoint: String, prompt: String) {
        guard !prompt.isEmpty, let s = ensure() else { return }
        generating = true
        Task.detached { [weak self] in
            let r = await s.work {
                $0.dispatch(["id": "tl.generateClip",
                             "endpoint": endpoint, "prompt": prompt])
            }
            await MainActor.run {
                guard let self else { return }
                self.generating = false
                if r["ok"] as? Bool == true {
                    self.error = nil
                    self.reloadState()
                } else {
                    self.error = r["error"] as? String ?? "generate failed"
                }
            }
        }
    }
}

struct MotionView: View {
    @EnvironmentObject var store: LibraryStore
    private var m: MotionStore { store.motion }
    @State private var genPrompt = ""
    @State private var genEndpoint = "http://127.0.0.1:8000"
    @State private var showGen = false

    var body: some View {
        HSplitView {
            VStack(spacing: 0) {
                previewPane
                Divider()
                timelinePane
            }
            inspector
                .frame(minWidth: 240, idealWidth: 280, maxWidth: 340)
        }
    }

    private var previewPane: some View {
        VStack(spacing: 6) {
            ZStack {
                Rectangle().fill(Color.black)
                if let img = m.preview {
                    Image(decorative: img, scale: 1).resizable().scaledToFit()
                } else {
                    Text("Add a clip").foregroundStyle(.secondary)
                }
            }
            .aspectRatio(16.0/9.0, contentMode: .fit)
            HStack(spacing: 10) {
                Text(fmt(m.playhead)).font(.system(size: 11, design: .monospaced))
                Slider(value: Binding(get: { m.playhead }, set: { m.playhead = $0 }),
                       in: 0...max(m.duration, 0.01),
                       onEditingChanged: { on in if !on { m.renderFrame() } })
                Text(fmt(m.duration)).font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(.secondary)
                Button("Export…") { m.exportMovie() }
                    .buttonStyle(.borderedProminent)
            }
            .padding(.horizontal, 12).padding(.bottom, 8)
        }
        .frame(minHeight: 300)
    }

    private func fmt(_ t: Double) -> String {
        let f = Int(max(m.fps, 1))
        let s = Int(t * Double(f))
        return String(format: "%02d:%02d.%02d", s / f / 60, (s / f) % 60, s % f)
    }

    private var timelinePane: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 6) {
                Button("+ Clip") { m.addClip() }
                Button("+ Audio") { m.addAudio() }
                Button("+ Subs") { m.addTrack(kind: "subtitle") }
                Spacer()
                if m.busy { ProgressView().controlSize(.small) }
                if let e = m.error { Text(e).foregroundStyle(.red).font(.caption).lineLimit(1) }
            }
            .padding(.horizontal, 10).padding(.vertical, 6)
            Divider()
            ScrollView {
                VStack(spacing: 2) {
                    ForEach(m.tracks) { t in
                        trackRow(t)
                    }
                }
                .padding(8)
            }
            .frame(minHeight: 140)
        }
        .frame(maxHeight: 300)
    }

    private func trackRow(_ t: MotionStore.TlTrack) -> some View {
        GeometryReader { geo in
            let pxPerSec = geo.size.width / max(m.duration, 1)
            ZStack(alignment: .leading) {
                RoundedRectangle(cornerRadius: 4)
                    .fill(Kou.bg2)
                    .overlay(alignment: .leading) {
                        Text(t.kind).font(.system(size: 9, weight: .medium))
                            .foregroundStyle(.secondary).padding(.leading, 6)
                    }
                ForEach(t.clips) { c in
                    RoundedRectangle(cornerRadius: 4)
                        .fill(m.selectedClip == c.id ? Kou.accent.opacity(0.8) : Color.gray.opacity(0.55))
                        .overlay(alignment: .leading) {
                            Text(c.label).font(.system(size: 9))
                                .lineLimit(1).padding(.leading, 5)
                                .foregroundStyle(.white)
                        }
                        .frame(width: max(8, c.dur * pxPerSec), height: 34)
                        .offset(x: c.offset * pxPerSec)
                        .onTapGesture { m.selectedClip = c.id }
                }
                ForEach(t.cues.indices, id: \.self) { i in
                    let c = t.cues[i]
                    let tt = (c["t"] as? NSNumber)?.doubleValue ?? 0
                    let dur = (c["dur"] as? NSNumber)?.doubleValue ?? 0.4
                    RoundedRectangle(cornerRadius: 2)
                        .fill(Color.yellow.opacity(0.6))
                        .frame(width: max(6, dur * pxPerSec), height: 12)
                        .offset(x: tt * pxPerSec, y: 11)
                }
                // playhead
                Rectangle().fill(Color.white.opacity(0.8)).frame(width: 1)
                    .offset(x: m.playhead * pxPerSec)
            }
        }
        .frame(height: 44)
    }

    private var inspector: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Clip").font(.headline)
            if let id = m.selectedClip,
               let c = m.tracks.flatMap(\.clips).first(where: { $0.id == id }) {
                LabeledContent("Source", value: c.label)
                kv("In", c.inPoint) { m.setClip(id, ["in": $0]) }
                kv("Out", c.outPoint) { m.setClip(id, ["out": $0]) }
                kv("Offset", c.offset) { m.setClip(id, ["offset": $0]) }
                kv("Fade in", c.fadeIn) { m.setClip(id, ["fadeIn": $0]) }
                kv("Fade out", c.fadeOut) { m.setClip(id, ["fadeOut": $0]) }
                Divider()
                Button("Detect Silence") { m.detectSilence() }
                if !m.silenceRanges.isEmpty {
                    ForEach(m.silenceRanges.indices, id: \.self) { i in
                        let r = m.silenceRanges[i]
                        Text(String(format: "silent %.1f–%.1fs", r.0, r.1))
                            .font(.caption).foregroundStyle(.secondary)
                    }
                }
                Divider()
                Button(m.transcribing ? "Transcribing…" : "Transcribe → Subtitles") {
                    m.transcribe()
                }
                .disabled(m.transcribing)
                Button("Remove Clip", role: .destructive) { m.removeClip() }
            } else {
                Text("Select a clip").foregroundStyle(.secondary)
            }
            Divider()
            DisclosureGroup("Generate clip (minimax-h3)") {
                TextField("Endpoint", text: $genEndpoint)
                    .textFieldStyle(.roundedBorder).font(.caption)
                TextField("Prompt", text: $genPrompt, axis: .vertical)
                    .textFieldStyle(.roundedBorder).font(.caption).lineLimit(3)
                Button(m.generating ? "Generating…" : "Generate") {
                    m.generateClip(endpoint: genEndpoint, prompt: genPrompt)
                }
                .disabled(m.generating || genPrompt.isEmpty)
            }
            Spacer()
        }
        .padding(12)
        .background(Kou.bg2)
    }

    private func kv(_ label: String, _ val: Double, commit: @escaping (Double) -> Void) -> some View {
        KvRow(label: label, val: val, commit: commit)
    }
}

/// label + editable number that commits on Return (local buffer so a half-typed
/// value doesn't dispatch per keystroke)
private struct KvRow: View {
    let label: String
    let val: Double
    let commit: (Double) -> Void
    @State private var text = ""
    var body: some View {
        HStack {
            Text(label).font(.caption).frame(width: 60, alignment: .leading)
            TextField("", text: $text)
                .textFieldStyle(.roundedBorder).font(.caption)
                .frame(width: 70)
                .onAppear { text = String(format: "%.2f", val) }
                .onChange(of: val) { _, v in text = String(format: "%.2f", v) }
                .onSubmit {
                    if let v = Double(text) { commit(v) }
                }
        }
    }
}
