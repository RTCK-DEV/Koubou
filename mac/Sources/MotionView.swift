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
        var volume: [[Double]]
        var transIn: TlTrans?
        var transOut: TlTrans?
        var rate: Double
        var eq: [Double]    // low, mid, high (dB)
        var comp: [Double]  // threshold, ratio, attack, release, makeup
        var dur: Double { max(0.01, outPoint - inPoint) }
        var label: String { src.isEmpty ? "clip" : URL(fileURLWithPath: src).deletingPathExtension().lastPathComponent }
    }
    /// one transition edge (`transIn`/`transOut` on the clip)
    struct TlTrans {
        var kind: String // slide | wipe | dip
        var dur: Double
        var color: [Double]? // dip colour, rgba 0..1

        /// JSON params for tl.setClip — nil omits `color`
        var params: [String: Any] {
            var p: [String: Any] = ["type": kind, "dur": dur]
            if let color { p["color"] = color }
            return p
        }
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
                    // preview = state refresh + frame render (reloadState
                    // already tails into renderFrame via applyState)
                    case .preview: self.reloadState()
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
                    inPoint: (c["inPoint"] as? NSNumber)?.doubleValue ?? 0,
                    outPoint: (c["outPoint"] as? NSNumber)?.doubleValue ?? 1,
                    offset: (c["offset"] as? NSNumber)?.doubleValue ?? 0,
                    opacity: (c["opacity"] as? [[NSNumber]] ?? []).map { $0.map { $0.doubleValue } },
                    scale: (c["scale"] as? [[NSNumber]] ?? []).map { $0.map { $0.doubleValue } },
                    fadeIn: (c["fadeIn"] as? NSNumber)?.doubleValue ?? 0,
                    fadeOut: (c["fadeOut"] as? NSNumber)?.doubleValue ?? 0,
                    volume: (c["volume"] as? [[NSNumber]] ?? []).map { $0.map { $0.doubleValue } },
                    transIn: parseTrans(c["transIn"]),
                    transOut: parseTrans(c["transOut"]),
                    rate: (c["rate"] as? NSNumber)?.doubleValue ?? 1.0,
                    eq: ["low", "mid", "high"].compactMap {
                        ((c["eq"] as? [String: Any])?[$0] as? NSNumber)?.doubleValue
                    },
                    comp: ["threshold", "ratio", "attack", "release", "makeup"].compactMap {
                        ((c["comp"] as? [String: Any])?[$0] as? NSNumber)?.doubleValue
                    })
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

    /// remove the selected clip and close the gap on its track
    func rippleDelete() {
        guard let id = selectedClip else { return }
        dispatch(["id": "tl.rippleDelete", "clip": id])
        selectedClip = nil
    }

    /// trim the selected clip edge by delta seconds (positive extends)
    func trimClip(edge: String, delta: Double) {
        guard let id = selectedClip else { return }
        dispatch(["id": "tl.trim", "clip": id, "edge": edge, "delta": delta],
                 then: .preview)
    }

    /// push one EQ band on the selected clip (dB); sends the full 3-band
    /// dict so partial edits don't drop other bands
    func setEq(band: Int, gain: Double) {
        guard let id = selectedClip,
              let c = tracks.flatMap(\.clips).first(where: { $0.id == id }) else { return }
        var e = c.eq
        while e.count < 3 { e.append(0) }
        e[band] = gain
        setClip(id, ["eq": ["low": e[0], "mid": e[1], "high": e[2]]])
    }

    /// push one compressor field on the selected clip
    func setComp(field: Int, value: Double) {
        guard let id = selectedClip,
              let c = tracks.flatMap(\.clips).first(where: { $0.id == id }) else { return }
        var k = c.comp.isEmpty ? [-18.0, 4.0, 5.0, 120.0, 0.0] : c.comp
        while k.count < 5 { k.append(0) }
        k[field] = value
        setClip(id, ["comp": ["threshold": k[0], "ratio": k[1], "attack": k[2],
                              "release": k[3], "makeup": k[4]]])
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
                // tl.detectSilence returns a bare [{start, end}] array
                if let ranges = r["result"] as? [[String: Any]] {
                    self?.silenceRanges = ranges.map {
                        (($0["start"] as? NSNumber)?.doubleValue ?? 0,
                         ($0["end"] as? NSNumber)?.doubleValue ?? 0)
                    }
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
    /// `track` is optional engine-side (defaults to the first video track),
    /// but pass the known index so the clip lands where the user expects.
    func generateClip(endpoint: String, prompt: String) {
        guard !prompt.isEmpty, let s = ensure() else { return }
        generating = true
        var cmd: [String: Any] = ["id": "tl.generateClip",
                                  "endpoint": endpoint, "prompt": prompt]
        if let vt = tracks.firstIndex(where: { $0.kind == "video" }) {
            cmd["track"] = vt
        }
        Task.detached { [weak self] in
            let r = await s.work { $0.dispatch(cmd) }
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

    /// index of the track holding `clipId` (track params are array indices)
    func trackIndex(of clipId: UInt64) -> Int? {
        tracks.firstIndex(where: { $0.clips.contains(where: { $0.id == clipId }) })
    }

    /// duck the selected clip's track under subtitle cues (tl.duck)
    func duckSelected(amount: Double = 0.25) {
        guard let id = selectedClip, let ti = trackIndex(of: id) else {
            error = "select a clip to duck"
            return
        }
        guard tracks[ti].kind != "subtitle" else {
            error = "select a video/audio clip — subtitles can't be ducked"
            return
        }
        dispatch(["id": "tl.duck", "track": ti, "amount": amount], then: .preview)
    }

    /// rewrite the whole volume keyframe list on a clip
    func setVolume(_ id: UInt64, _ kfs: [[Double]]) {
        setClip(id, ["volume": kfs])
    }

    /// set/clear a transition edge; nil clears
    func setTransition(_ id: UInt64, key: String, _ tr: TlTrans?) {
        setClip(id, [key: tr?.params as Any ?? NSNull()])
    }

    private func parseTrans(_ any: Any?) -> TlTrans? {
        guard let d = any as? [String: Any],
              let kind = d["type"] as? String else { return nil }
        let dur = (d["dur"] as? NSNumber)?.doubleValue ?? 0.5
        let color = (d["color"] as? [NSNumber])?.map { $0.doubleValue }
        return TlTrans(kind: kind, dur: dur, color: color)
    }
}

struct MotionView: View {
    @ObservedObject var m: MotionStore
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
        ScrollView {
            VStack(alignment: .leading, spacing: 10) {
                if let id = m.selectedClip,
                   let c = m.tracks.flatMap(\.clips).first(where: { $0.id == id }) {
                    Panel("Clip") {
                        LabeledContent("Source", value: c.label).font(.caption)
                        kv("In", c.inPoint) { m.setClip(id, ["in": $0]) }
                        kv("Out", c.outPoint) { m.setClip(id, ["out": $0]) }
                        kv("Offset", c.offset) { m.setClip(id, ["offset": $0]) }
                        kv("Fade in", c.fadeIn) { m.setClip(id, ["fadeIn": $0]) }
                        kv("Fade out", c.fadeOut) { m.setClip(id, ["fadeOut": $0]) }
                        kv("Rate", c.rate) { m.setClip(id, ["rate": $0]) }
                        HStack(spacing: 6) {
                            Text("Trim").font(.caption).foregroundStyle(.secondary)
                            ForEach([("in", "In"), ("out", "Out")], id: \.0) { (edge, lab) in
                                Button("\(lab) −") { m.trimClip(edge: edge, delta: -0.5) }
                                    .buttonStyle(KouSecondaryButton())
                                Button("\(lab) +") { m.trimClip(edge: edge, delta: 0.5) }
                                    .buttonStyle(KouSecondaryButton())
                            }
                        }
                    }
                    Panel("Transitions") {
                        transitionEdge(id, edge: "In", key: "transIn", cur: c.transIn)
                        transitionEdge(id, edge: "Out", key: "transOut", cur: c.transOut)
                    }
                    Panel("Audio") {
                        volumeEditor(c)
                        Text("EQ (dB)").font(.caption2).foregroundStyle(.secondary)
                        HStack(spacing: 6) {
                            ForEach(0..<3, id: \.self) { i in
                                let lab = ["Lo", "Mid", "Hi"][i]
                                kv(lab, c.eq.indices.contains(i) ? c.eq[i] : 0) {
                                    m.setEq(band: i, gain: $0)
                                }
                            }
                        }
                        Text("Compressor").font(.caption2).foregroundStyle(.secondary)
                        HStack(spacing: 6) {
                            kv("Thr", c.comp.indices.contains(0) ? c.comp[0] : -18) {
                                m.setComp(field: 0, value: $0)
                            }
                            kv("Ratio", c.comp.indices.contains(1) ? c.comp[1] : 4) {
                                m.setComp(field: 1, value: $0)
                            }
                        }
                        ToolChip(label: "Duck under subtitles", icon: "chevron.down.circle") {
                            m.duckSelected()
                        }
                    }
                    Panel("Tools") {
                        HStack(spacing: 6) {
                            ToolChip(label: "Detect Silence", icon: "waveform") { m.detectSilence() }
                            ToolChip(label: m.transcribing ? "Transcribing…" : "Transcribe → Subs",
                                     icon: "text.bubble") { m.transcribe() }
                                .disabled(m.transcribing)
                        }
                        if !m.silenceRanges.isEmpty {
                            ForEach(m.silenceRanges.indices, id: \.self) { i in
                                let r = m.silenceRanges[i]
                                Text(String(format: "silent %.1f–%.1fs", r.0, r.1))
                                    .font(.caption).foregroundStyle(.secondary)
                            }
                        }
                        Button("Remove Clip", role: .destructive) { m.removeClip() }
                            .buttonStyle(KouSecondaryButton())
                        Button("Ripple Delete", role: .destructive) { m.rippleDelete() }
                            .buttonStyle(KouSecondaryButton())
                    }
                } else {
                    Panel("Clip") {
                        Text("Select a clip").font(.caption).foregroundStyle(.secondary)
                    }
                }
                Panel("Generate clip (minimax-h3)") {
                    TextField("Endpoint", text: $genEndpoint)
                        .textFieldStyle(.roundedBorder).font(.caption)
                    TextField("Prompt", text: $genPrompt, axis: .vertical)
                        .textFieldStyle(.roundedBorder).font(.caption).lineLimit(3)
                    Button(m.generating ? "Generating…" : "Generate") {
                        m.generateClip(endpoint: genEndpoint, prompt: genPrompt)
                    }
                    .buttonStyle(KouPrimaryButton())
                    .disabled(m.generating || genPrompt.isEmpty)
                }
            }
            .padding(10)
        }
        .background(Kou.bg0)
    }

    /// None/Slide/Wipe/Dip picker + duration (+ dip colour for dips)
    private func transitionEdge(_ id: UInt64, edge: String, key: String,
                                cur: MotionStore.TlTrans?) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(edge).font(.caption).foregroundStyle(Kou.text2)
                    .frame(width: 24, alignment: .leading)
                SegPicker([( "", "None" ), ("slide", "Slide"), ("wipe", "Wipe"), ("dip", "Dip")],
                          selection: Binding(
                              get: { cur?.kind ?? "" },
                              set: { nk in
                                  if nk.isEmpty {
                                      m.setTransition(id, key: key, nil)
                                  } else {
                                      var t = MotionStore.TlTrans(kind: nk, dur: cur?.dur ?? 0.5, color: cur?.color)
                                      if nk == "dip" && t.color == nil { t.color = [0, 0, 0, 1] }
                                      m.setTransition(id, key: key, t)
                                  }
                              }))
            }
            if let cur {
                kv("Dur s", cur.dur) { d in
                    var t = cur
                    t.dur = max(0, d)
                    m.setTransition(id, key: key, t)
                }
                if cur.kind == "dip" {
                    dipColorRow(id, key: key, cur: cur)
                }
            }
        }
    }

    /// dip colour swatches: a few presets + free pick
    private func dipColorRow(_ id: UInt64, key: String,
                             cur: MotionStore.TlTrans) -> some View {
        HStack(spacing: 6) {
            Text("Colour").font(.caption).foregroundStyle(Kou.text2)
                .frame(width: 40, alignment: .leading)
            ForEach([[0.0, 0.0, 0.0], [1.0, 1.0, 1.0], [0.85, 0.3, 0.25]], id: \.self) { rgb in
                Button {
                    var t = cur
                    t.color = [rgb[0], rgb[1], rgb[2], 1.0]
                    m.setTransition(id, key: key, t)
                } label: {
                    Circle()
                        .fill(Color(red: rgb[0], green: rgb[1], blue: rgb[2]))
                        .frame(width: 14, height: 14)
                        .overlay(Circle().stroke(Kou.border, lineWidth: 0.5))
                }
                .buttonStyle(.plain)
            }
            ColorPicker("", selection: Binding(
                get: {
                    let c = cur.color ?? [0, 0, 0, 1]
                    return Color(red: c[0], green: c[1], blue: c[2])
                },
                set: { col in
                    let ns = NSColor(col).usingColorSpace(.sRGB) ?? .black
                    var t = cur
                    t.color = [ns.redComponent, ns.greenComponent, ns.blueComponent, 1.0]
                    m.setTransition(id, key: key, t)
                }))
                .labelsHidden().frame(width: 24)
        }
    }

    /// minimal volume keyframe list: t → gain rows with add/remove
    private func volumeEditor(_ c: MotionStore.TlClip) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text("Volume kf").font(.caption).foregroundStyle(Kou.text2)
                Spacer()
                IconAction(icon: "plus", label: "at playhead") {
                    let t = max(0, m.playhead - c.offset)
                    var kfs = c.volume
                    kfs.append([t, 1.0])
                    kfs.sort { $0[0] < $1[0] }
                    m.setVolume(c.id, kfs)
                }
            }
            if c.volume.isEmpty {
                Text("unity gain").font(.caption2).foregroundStyle(Kou.text3)
            }
            ForEach(Array(c.volume.enumerated()), id: \.offset) { i, kf in
                HStack(spacing: 6) {
                    Text(String(format: "%.2fs", kf[0]))
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(Kou.text2)
                        .frame(width: 44, alignment: .leading)
                    Slider(value: Binding(
                        get: { kf[1] },
                        set: { v in
                            var kfs = c.volume
                            kfs[i][1] = v
                            m.setVolume(c.id, kfs)
                        }), in: 0...2)
                    Text(String(format: "%.2f", kf[1]))
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(Kou.text3)
                        .frame(width: 34, alignment: .trailing)
                    IconAction(icon: "minus") {
                        var kfs = c.volume
                        kfs.remove(at: i)
                        m.setVolume(c.id, kfs)
                    }
                }
            }
        }
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
