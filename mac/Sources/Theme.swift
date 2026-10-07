import SwiftUI

/// koubou "darkroom" theme — near-black surfaces, warm amber accent.
enum Kou {
    static let bg0 = Color(red: 0.035, green: 0.035, blue: 0.042)   // stage / window
    static let bg1 = Color(red: 0.075, green: 0.077, blue: 0.086)   // sidebar / inspector
    static let bg2 = Color(red: 0.115, green: 0.118, blue: 0.130)   // cards / panels
    static let bg3 = Color(red: 0.165, green: 0.170, blue: 0.185)   // inset tracks / chips
    static let bg4 = Color(red: 0.22, green: 0.225, blue: 0.245)    // hover / selected chip
    static let hairline = Color.white.opacity(0.07)
    static let border = Color.white.opacity(0.10)
    static let text1 = Color.white.opacity(0.92)
    static let text2 = Color.white.opacity(0.55)
    static let text3 = Color.white.opacity(0.32)
    static let accent = Color(red: 0.96, green: 0.66, blue: 0.24)   // warm amber
    static let accentSoft = accent.opacity(0.16)
    static let gold = Color(red: 1.0, green: 0.80, blue: 0.30)
    static let track = Color.white.opacity(0.13)

    /// Gradient slider tracks (RawTherapee/DaVinci coloured rails): temp is
    /// blue→neutral→amber, tint green→neutral→magenta, hue a rainbow strip.
    static let tempTrack = LinearGradient(colors: [
        Color(red: 0.30, green: 0.52, blue: 0.95),
        Color(red: 0.80, green: 0.80, blue: 0.82),
        Color(red: 0.98, green: 0.62, blue: 0.20),
    ], startPoint: .leading, endPoint: .trailing)
    static let tintTrack = LinearGradient(colors: [
        Color(red: 0.25, green: 0.75, blue: 0.40),
        Color(red: 0.80, green: 0.80, blue: 0.82),
        Color(red: 0.90, green: 0.30, blue: 0.70),
    ], startPoint: .leading, endPoint: .trailing)
    static let hueTrack = LinearGradient(colors: [
        .red, .orange, .yellow, .green, .cyan, .blue,
        Color(hue: 0.83, saturation: 0.85, brightness: 0.95), .red,
    ], startPoint: .leading, endPoint: .trailing)
}

/// Custom slider track: thin rail, amber fill drawn from the default (reset)
/// position toward the knob, white knob ringed with accent. Drag or click to
/// set; double-click resets; hovering adjusts with the scroll wheel
/// (DaVinci/Lightroom behaviour). `reset` defaults to 0 when the range
/// contains it.
struct TrackSlider: View {
    @Binding var value: Double
    let range: ClosedRange<Double>
    var step: Double = 0.01
    var reset: Double? = nil
    var height: CGFloat = 16
    /// optional gradient rail (temp/tint/hue); plain track colour when nil
    var track: LinearGradient? = nil

    @State private var dragActive = false
    @State private var lastStart = Date.distantPast
    @State private var scrollMon = SliderScrollMonitor()

    private var def: Double {
        reset ?? (range.contains(0) ? 0 : range.lowerBound)
    }

    var body: some View {
        GeometryReader { geo in
            let w = geo.size.width
            let span = range.upperBound - range.lowerBound
            let frac = { (v: Double) -> CGFloat in
                CGFloat((v - range.lowerBound) / span).clampedTo01() * w
            }
            let kx = frac(value)
            let dx = frac(def)
            ZStack {
                if let track {
                    Capsule().fill(track)
                        .frame(width: w, height: 5)
                        .overlay(Capsule().stroke(Kou.border, lineWidth: 0.5))
                } else {
                    Capsule().fill(Kou.track)
                        .frame(width: w, height: 4)
                }
                Capsule().fill(track == nil ? Kou.accent.opacity(0.85) : Color.white.opacity(0.55))
                    .frame(width: max(abs(kx - dx), 2), height: track == nil ? 4 : 5)
                    .offset(x: min(kx, dx))
                Circle()
                    .fill(Color.white)
                    .frame(width: 11, height: 11)
                    .overlay(Circle().stroke(Kou.accent.opacity(0.95), lineWidth: 2))
                    .shadow(color: .black.opacity(0.5), radius: 1, y: 0.5)
                    .position(x: kx.clamped(to: 5.5...(w - 5.5)), y: geo.size.height / 2)
            }
            .frame(maxHeight: .infinity)
            .contentShape(Rectangle())
            // DragGesture(minimumDistance:0) swallows onTapGesture, so the
            // double-click reset is detected manually: two gesture starts
            // within 0.3s restore the default instead of jumping position.
            .gesture(DragGesture(minimumDistance: 0)
                .onChanged { g in
                    if !dragActive {
                        dragActive = true
                        if g.time.timeIntervalSince(lastStart) < 0.3 {
                            value = def
                            lastStart = .distantPast
                            return
                        }
                        lastStart = g.time
                    }
                    let f = (Double(g.location.x) / Double(w)).clamped(to: 0...1)
                    var v = range.lowerBound + f * span
                    v = (v / step).rounded() * step
                    value = v.clamped(to: range)
                }
                .onEnded { _ in dragActive = false })
            .onHover { h in
                if h {
                    scrollMon.handler = { ev in
                        let d = Double(ev.scrollingDeltaY + ev.scrollingDeltaX)
                        let span = range.upperBound - range.lowerBound
                        // ~200 scroll-units traverse the full range; snapped to step
                        value = (((value + d * span / 200) / step).rounded() * step)
                            .clamped(to: range)
                        return false   // consumed — don't scroll the panel too
                    }
                    scrollMon.install()
                } else {
                    scrollMon.uninstall()
                }
            }
            .onDisappear { scrollMon.uninstall() }
        }
        .frame(height: height)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("slider")
        .accessibilityValue(String(format: "%.2f", value))
    }
}

/// Per-slider scroll monitor, installed only while hovered.
final class SliderScrollMonitor {
    private var monitor: Any?
    /// true = let the event pass through; false = consumed
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
    deinit { uninstall() }
}

/// Monospaced numeric readout that turns into an editable TextField on click
/// (DaVinci numeric entry). Commits on Return / focus loss; Escape cancels.
struct NumValue: View {
    @Binding var value: Double
    let range: ClosedRange<Double>
    var width: CGFloat = 38
    var reset: Double? = nil
    var step: Double = 0.01
    var digits: Int = 2
    @State private var editing = false
    @State private var text = ""
    @FocusState private var focus: Bool

    private var def: Double {
        reset ?? (range.contains(0) ? 0 : range.lowerBound)
    }

    var body: some View {
        Group {
            if editing {
                // field starts empty, placeholder shows the current value —
                // typing a number replaces it outright (DaVinci-style entry)
                TextField(String(format: "%.\(digits)f", value), text: $text)
                    .textFieldStyle(.plain)
                    .font(.system(size: 10.5).monospacedDigit())
                    .foregroundStyle(Kou.accent)
                    .multilineTextAlignment(.trailing)
                    .frame(width: width + 8, alignment: .trailing)
                    .focused($focus)
                    .onSubmit(commit)
                    .onExitCommand { editing = false }
                    .onChange(of: focus) { _, f in if !f { commit() } }
                    .onAppear { text = ""; focus = true }
            } else {
                Text(String(format: "%.\(digits)f", value))
                    .font(.system(size: 10.5).monospacedDigit())
                    .foregroundStyle(abs(value - def) < 1e-9 ? Kou.text3 : Kou.accent)
                    // generous hitbox: pads catch taps that would otherwise land
                    // on the track just left of the number
                    .frame(minWidth: width, alignment: .trailing)
                    .padding(.horizontal, 5).padding(.vertical, 3)
                    .background(Kou.bg3.opacity(0.001))   // invisible grab surface
                    .contentShape(Rectangle())
                    .onTapGesture(count: 1) { editing = true }
            }
        }
    }

    private func commit() {
        let t = text.trimmingCharacters(in: .whitespaces)
            .replacingOccurrences(of: ",", with: ".")
        if !t.isEmpty, let v = Double(t) {
            value = v.clamped(to: range)
        }
        editing = false
    }
}

/// Vertical slider for the zone equalizer: same drag + dblclick-reset +
/// scroll-adjust behaviour as TrackSlider, oriented bottom→top.
struct VSlider: View {
    @Binding var value: Double
    let range: ClosedRange<Double>
    var step: Double = 0.25
    var reset: Double = 0
    var height: CGFloat = 64

    @State private var dragActive = false
    @State private var lastStart = Date.distantPast
    @State private var scrollMon = SliderScrollMonitor()

    var body: some View {
        GeometryReader { geo in
            let h = geo.size.height
            let w = geo.size.width
            let span = range.upperBound - range.lowerBound
            let fy = { (v: Double) -> CGFloat in
                (1 - CGFloat((v - range.lowerBound) / span).clampedTo01()) * h
            }
            let ky = fy(value)
            let zy = fy(reset)
            ZStack {
                Capsule().fill(Kou.track)
                    .frame(width: 4, height: h)
                Capsule().fill(Kou.accent.opacity(0.85))
                    .frame(width: 4, height: max(abs(ky - zy), 2))
                    .offset(y: (min(ky, zy) + max(ky, zy)) / 2 - h / 2)
                Circle()
                    .fill(Color.white)
                    .frame(width: 9, height: 9)
                    .overlay(Circle().stroke(Kou.accent.opacity(0.95), lineWidth: 1.5))
                    .shadow(color: .black.opacity(0.5), radius: 1, y: 0.5)
                    .position(x: w / 2, y: ky.clamped(to: 4.5...(h - 4.5)))
            }
            .frame(width: w, height: h)
            .contentShape(Rectangle())
            .gesture(DragGesture(minimumDistance: 0)
                .onChanged { g in
                    if !dragActive {
                        dragActive = true
                        if g.time.timeIntervalSince(lastStart) < 0.3 {
                            value = reset
                            lastStart = .distantPast
                            return
                        }
                        lastStart = g.time
                    }
                    let f = (1 - Double(g.location.y / h)).clamped(to: 0...1)
                    let v = range.lowerBound + f * span
                    value = ((v / step).rounded() * step).clamped(to: range)
                }
                .onEnded { _ in dragActive = false })
            .onHover { h in
                if h {
                    scrollMon.handler = { ev in
                        let d = Double(ev.scrollingDeltaY + ev.scrollingDeltaX)
                        let v = value + d * span / 200
                        value = ((v / step).rounded() * step).clamped(to: range)
                        return false
                    }
                    scrollMon.install()
                } else {
                    scrollMon.uninstall()
                }
            }
            .onDisappear { scrollMon.uninstall() }
        }
        .frame(height: height)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("zone slider")
        .accessibilityValue(String(format: "%.2f", value))
    }
}

/// darktable tone equalizer: bank of 9 vertical EV sliders, one per
/// log2-luminance band centred at -4..+4 EV.
struct ZoneEQ: View {
    @Binding var zones: [Double]

    var body: some View {
        HStack(spacing: 4) {
            ForEach(0..<9, id: \.self) { i in
                VStack(spacing: 3) {
                    VSlider(value: Binding(
                        get: { i < zones.count ? zones[i] : 0 },
                        set: { v in
                            while zones.count < 9 { zones.append(0) }
                            zones[i] = v
                        }),
                        range: -4...4, step: 0.25, reset: 0, height: 62)
                    Text("\(i - 4)")
                        .font(.system(size: 7.5).monospacedDigit())
                        .foregroundStyle(zones.count > i && zones[i] != 0 ? Kou.accent : Kou.text3)
                }
                .frame(maxWidth: .infinity)
            }
        }
    }
}

/// Label + TrackSlider + editable monospaced value readout.
struct SliderRow: View {
    let title: String
    @Binding var value: Double
    let range: ClosedRange<Double>
    var step: Double = 0.01
    var reset: Double? = nil

    var track: LinearGradient? = nil

    init(_ title: String, _ value: Binding<Double>, _ range: ClosedRange<Double>,
         step: Double = 0.01, reset: Double? = nil, track: LinearGradient? = nil) {
        self.title = title
        self._value = value
        self.range = range
        self.step = step
        self.reset = reset
        self.track = track
    }

    var body: some View {
        HStack(spacing: 7) {
            Text(title)
                .font(.system(size: 10.5, weight: .medium))
                .foregroundStyle(Kou.text2)
                .frame(width: 60, alignment: .leading)
                .lineLimit(1)
            TrackSlider(value: $value, range: range, step: step, reset: reset, track: track)
            NumValue(value: $value, range: range, reset: reset, step: step)
        }
        .frame(height: 20)
    }
}

/// DaVinci qualifier range bar: gradient track with two draggable handles
/// setting [lo, hi]. `gradient` supplies the strip's colours.
struct RangeBar: View {
    let title: String
    @Binding var lo: Double
    @Binding var hi: Double
    let gradient: LinearGradient
    var height: CGFloat = 14

    var body: some View {
        HStack(spacing: 7) {
            Text(title)
                .font(.system(size: 10.5, weight: .medium))
                .foregroundStyle(Kou.text2)
                .frame(width: 60, alignment: .leading)
                .lineLimit(1)
            GeometryReader { geo in
                let w = geo.size.width
                let lx = CGFloat(lo.clamped(to: 0...1)) * w
                let hx = CGFloat(hi.clamped(to: 0...1)) * w
                ZStack {
                    Capsule().fill(gradient).frame(width: w, height: 8)
                        .overlay(Capsule().stroke(Kou.border, lineWidth: 0.5))
                    // dim outside the selected range
                    HStack(spacing: 0) {
                        Rectangle().fill(Kou.bg0.opacity(0.55)).frame(width: max(lx, 0))
                        Spacer(minLength: 0)
                        Rectangle().fill(Kou.bg0.opacity(0.55)).frame(width: max(w - hx, 0))
                    }
                    .clipShape(Capsule())
                    .frame(width: w, height: 8)
                    barHandle(x: lx, y: geo.size.height / 2, max: w)
                    barHandle(x: hx, y: geo.size.height / 2, max: w)
                }
                .contentShape(Rectangle())
                .gesture(DragGesture(minimumDistance: 0).onChanged { g in
                    let f = Double(g.location.x / w).clamped(to: 0...1)
                    if abs(g.startLocation.x - lx) <= abs(g.startLocation.x - hx) {
                        lo = min(f, hi)
                    } else {
                        hi = max(f, lo)
                    }
                })
            }
            .frame(height: height)
        }
        .frame(height: 20)
    }

    private func barHandle(x: CGFloat, y: CGFloat, max w: CGFloat) -> some View {
        RoundedRectangle(cornerRadius: 2)
            .fill(Color.white)
            .frame(width: 5, height: 12)
            .overlay(RoundedRectangle(cornerRadius: 2).stroke(Kou.accent, lineWidth: 1))
            .shadow(color: .black.opacity(0.5), radius: 1, y: 0.5)
            .position(x: x.clamped(to: 2...max(w - 2, 2)), y: y)
    }
}

/// Card panel replacing GroupBox: small-caps hairline-separated header,
/// optional collapse chevron when `expanded` is bound.
struct Panel<Content: View, Trailing: View>: View {
    let title: String
    var expanded: Binding<Bool>?
    let content: Content
    let trailing: Trailing

    init(_ title: String,
         expanded: Binding<Bool>? = nil,
         @ViewBuilder trailing: () -> Trailing = { EmptyView() },
         @ViewBuilder content: () -> Content) {
        self.title = title
        self.expanded = expanded
        self.trailing = trailing()
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 6) {
                if expanded != nil {
                    Image(systemName: "chevron.right")
                        .font(.system(size: 8, weight: .bold))
                        .foregroundStyle(Kou.text3)
                        .rotationEffect(.degrees(expanded?.wrappedValue == true ? 90 : 0))
                }
                Text(title.uppercased())
                    .font(.system(size: 9.5, weight: .semibold))
                    .tracking(1.2)
                    .foregroundStyle(Kou.text2)
                Spacer()
                trailing
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 8)
            .contentShape(Rectangle())
            .onTapGesture {
                guard expanded != nil else { return }
                withAnimation(.easeInOut(duration: 0.15)) {
                    expanded?.wrappedValue.toggle()
                }
            }

            if expanded?.wrappedValue != false {
                VStack(spacing: 8) { content }
                    .padding(.horizontal, 10)
                    .padding(.bottom, 10)
                    .padding(.top, 2)
            }
        }
        .background(Kou.bg2)
        .clipShape(RoundedRectangle(cornerRadius: 8))
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(Kou.hairline, lineWidth: 1))
    }
}

extension Panel where Trailing == EmptyView {
    init(_ title: String, expanded: Binding<Bool>? = nil,
         @ViewBuilder content: () -> Content) {
        self.init(title, expanded: expanded, trailing: { EmptyView() }, content: content)
    }
}

/// Capsule segmented control (replaces .pickerStyle(.segmented)).
struct SegPicker<T: Hashable>: View {
    let options: [(value: T, label: String)]
    @Binding var selection: T

    init(_ options: [(T, String)], selection: Binding<T>) {
        self.options = options
        self._selection = selection
    }

    var body: some View {
        HStack(spacing: 0) {
            ForEach(options, id: \.value) { opt in
                Button { selection = opt.value } label: {
                    Text(opt.label)
                        .font(.system(size: 10, weight: selection == opt.value ? .semibold : .regular))
                        .foregroundStyle(selection == opt.value ? .white : Kou.text2)
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 5)
                        .background(
                            Capsule().fill(selection == opt.value ? Kou.bg4 : .clear)
                                .overlay(Capsule().stroke(
                                    selection == opt.value ? Kou.border : .clear, lineWidth: 0.5))
                        )
                        .padding(2)
                }
                .buttonStyle(.plain)
            }
        }
        .padding(1)
        .background(Kou.bg3)
        .clipShape(Capsule())
    }
}

struct KouPrimaryButton: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 11, weight: .semibold))
            .foregroundStyle(Color.black.opacity(0.85))
            .padding(.horizontal, 12).padding(.vertical, 5)
            .background(Capsule().fill(Kou.accent))
            .opacity(configuration.isPressed ? 0.75 : 1)
    }
}

struct KouSecondaryButton: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 11, weight: .medium))
            .foregroundStyle(Kou.text1)
            .padding(.horizontal, 11).padding(.vertical, 5)
            .background(Capsule().fill(Kou.bg3)
                .overlay(Capsule().stroke(Kou.border, lineWidth: 0.5)))
            .opacity(configuration.isPressed ? 0.7 : 1)
    }
}

/// Small icon button for toolbars / action strips.
struct IconAction: View {
    let icon: String
    var label: String? = nil
    var active = false
    var action: () -> Void
    var body: some View {
        Button(action: action) {
            HStack(spacing: 4) {
                Image(systemName: icon).font(.system(size: 11, weight: .medium))
                if let label { Text(label).font(.system(size: 10.5, weight: .medium)) }
            }
            .foregroundStyle(active ? Kou.accent : Kou.text2)
            .padding(.horizontal, label != nil ? 8 : 6)
            .padding(.vertical, 5)
            .background(RoundedRectangle(cornerRadius: 6).fill(active ? Kou.accentSoft : Kou.bg3)
                .overlay(RoundedRectangle(cornerRadius: 6).stroke(active ? Kou.accent.opacity(0.4) : Kou.hairline, lineWidth: 0.5)))
        }
        .buttonStyle(.plain)
    }
}

/// Small tool button used inside panels ("Pick WB on image" etc).
struct ToolChip: View {
    let label: String
    var icon: String? = nil
    var active = false
    var action: () -> Void
    var body: some View {
        Button(action: action) {
            HStack(spacing: 4) {
                if let icon { Image(systemName: icon).font(.system(size: 9, weight: .bold)) }
                Text(label).font(.system(size: 10, weight: .medium))
            }
            .foregroundStyle(active ? Kou.accent : Kou.text1)
            .padding(.horizontal, 8).padding(.vertical, 4)
            .background(Capsule().fill(active ? Kou.accentSoft : Kou.bg3)
                .overlay(Capsule().stroke(active ? Kou.accent.opacity(0.5) : Kou.hairline, lineWidth: 0.5)))
        }
        .buttonStyle(.plain)
    }
}

extension Double {
    func clamped(to r: ClosedRange<Double>) -> Double { Swift.min(Swift.max(self, r.lowerBound), r.upperBound) }
}

extension CGFloat {
    func clampedTo01() -> CGFloat { Swift.min(Swift.max(self, 0), 1) }
    func clamped(to r: ClosedRange<CGFloat>) -> CGFloat { Swift.min(Swift.max(self, r.lowerBound), r.upperBound) }
}
