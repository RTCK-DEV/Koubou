import Foundation
import AppKit
import CoreGraphics

final class KouEngine: @unchecked Sendable {
    private let handle: UnsafeMutableRawPointer
    private let queue = DispatchQueue(label: "ara.engine", qos: .userInitiated)

    static let shared: KouEngine = {
        guard let e = KouEngine() else { fatalError("araware_init failed") }
        return e
    }()

    private init?() {
        guard let h = araware_init() else { return nil }
        handle = h
    }

    deinit { araware_free_engine(handle) }

    var lastError: String {
        guard let p = araware_last_error() else { return "" }
        defer { araware_free_string(p) }
        return String(cString: p)
    }

    private func takeString(_ p: UnsafeMutablePointer<CChar>?) -> String? {
        guard let p else { return nil }
        defer { araware_free_string(p) }
        return String(cString: p)
    }

    private func cgImage(_ img: AraImage) -> CGImage? {
        guard img.data != nil, img.width > 0, img.height > 0 else { return nil }
        let ctx = UnsafeMutablePointer<AraImage>.allocate(capacity: 1)
        ctx.initialize(to: img)
        let w = Int(img.width), h = Int(img.height), len = img.len
        guard let provider = CGDataProvider(
            dataInfo: ctx,
            data: img.data,
            size: Int(len),
            releaseData: { info, _, _ in
                guard let info else { return }
                let i = info.assumingMemoryBound(to: AraImage.self)
                araware_free_image(i.move())
                i.deallocate()
            }
        ) else {
            araware_free_image(ctx.move())
            ctx.deallocate()
            return nil
        }
        return CGImage(
            width: w, height: h,
            bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: w * 4,
            space: CGColorSpace(name: CGColorSpace.sRGB) ?? CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.noneSkipLast.rawValue),
            provider: provider, decode: nil,
            shouldInterpolate: true, intent: .defaultIntent
        )
    }

    /// Run blocking engine calls off the main thread.
    func work<T>(_ f: @escaping (KouEngine) -> T) async -> T {
        await withCheckedContinuation { c in
            queue.async { c.resume(returning: f(self)) }
        }
    }

    func scan(folder: String) -> [Photo] {
        guard let js = takeString(folder.withCString { araware_scan_folder(handle, $0) }),
              let data = js.data(using: .utf8),
              let photos = try? JSONDecoder().decode([Photo].self, from: data)
        else { return [] }
        return photos
    }

    func thumbnail(path: String, maxPx: UInt32 = 512) -> CGImage? {
        cgImage(path.withCString { araware_thumbnail(handle, $0, maxPx) })
    }

    /// Render plus the output-image histogram (R,G,B,luma x 256).
    func render(path: String, recipe: Recipe, maxPx: UInt32) -> (CGImage?, [[UInt32]]) {
        let js = (try? JSONEncoder().encode(recipe)).flatMap { String(data: $0, encoding: .utf8) } ?? ""
        var bins = [UInt32](repeating: 0, count: 1024)
        let img = bins.withUnsafeMutableBufferPointer { buf in
            buf.baseAddress!.withMemoryRebound(to: AraHistogram.self, capacity: 1) { hist in
                js.withCString { r in
                    path.withCString { araware_render_h(handle, $0, r, maxPx, hist) }
                }
            }
        }
        let rows = (0..<4).map { ch in Array(bins[(ch * 256)..<(ch * 256 + 256)]) }
        return (cgImage(img), rows)
    }

    /// Render plus scope buffers (histogram, waveform 3ch, vectorscope, CIE xy).
    func renderScopes(path: String, recipe: Recipe, maxPx: UInt32)
        -> (CGImage?, [UInt32], [UInt32], [UInt32], [UInt32])
    {
        let js = (try? JSONEncoder().encode(recipe)).flatMap { String(data: $0, encoding: .utf8) } ?? ""
        var wave = [UInt32](repeating: 0, count: 196608)
        var vec = [UInt32](repeating: 0, count: 65536)
        var cie = [UInt32](repeating: 0, count: 65536)
        var bins = [UInt32](repeating: 0, count: 1024)
        let img = wave.withUnsafeMutableBufferPointer { wv in
            vec.withUnsafeMutableBufferPointer { vc in
                cie.withUnsafeMutableBufferPointer { ce in
                    bins.withUnsafeMutableBufferPointer { hs in
                        js.withCString { r in
                            path.withCString {
                                araware_scopes(handle, $0, r, maxPx,
                                               wv.baseAddress, vc.baseAddress,
                                               ce.baseAddress, hs.baseAddress)
                            }
                        }
                    }
                }
            }
        }
        return (cgImage(img), bins, wave, vec, cie)
    }

    func export(path: String, recipe: Recipe) -> CGImage? {
        let js = (try? JSONEncoder().encode(recipe)).flatMap { String(data: $0, encoding: .utf8) } ?? ""
        return cgImage(js.withCString { r in path.withCString { araware_export(handle, $0, r) } })
    }

    func metadata(path: String) -> String {
        takeString(path.withCString { araware_metadata(handle, $0) }) ?? ""
    }

    /// Auto-correction analysis on a neutral preview — returns suggested
    /// recipe values plus confidence fields (see core/src/auto.rs).
    func autoAnalyze(path: String) -> AutoSuggestion? {
        guard let js = takeString(path.withCString { araware_auto_analyze(handle, $0) }),
              let data = js.data(using: .utf8)
        else { return nil }
        return try? JSONDecoder().decode(AutoSuggestion.self, from: data)
    }

    func sidecar(path: String) -> Sidecar {
        guard let js = takeString(path.withCString { araware_sidecar_read($0) }),
              let data = js.data(using: .utf8),
              let sc = try? JSONDecoder().decode(Sidecar.self, from: data)
        else { return Sidecar() }
        return sc
    }

    @discardableResult
    func writeSidecar(path: String, _ sc: Sidecar) -> Bool {
        guard let data = try? JSONEncoder().encode(sc),
              let js = String(data: data, encoding: .utf8) else { return false }
        return js.withCString { j in path.withCString { araware_sidecar_write($0, j) } } == 0
    }

    @discardableResult
    func setRating(path: String, _ rating: Int) -> Bool {
        path.withCString { araware_set_rating(handle, $0, Int32(rating)) } == 0
    }

    @discardableResult
    func setLabel(path: String, _ label: String) -> Bool {
        label.withCString { l in path.withCString { araware_set_label(handle, $0, l) } } == 0
    }
}

/// A layered-document session (koubou-composer): JSON commands in,
/// JSON responses out — the same dispatcher as the CLI control channel
/// and the MCP server.
final class DocSession: @unchecked Sendable {
    private let handle: UnsafeMutableRawPointer
    private let queue = DispatchQueue(label: "kou.doc", qos: .userInitiated)

    init?() {
        guard let h = kou_session_new() else { return nil }
        handle = h
    }

    deinit { kou_session_free(handle) }

    /// Synchronous dispatch — call off the main thread (via `work`).
    func dispatch(_ cmd: [String: Any]) -> [String: Any] {
        guard JSONSerialization.isValidJSONObject(cmd),
              let data = try? JSONSerialization.data(withJSONObject: cmd),
              let js = String(data: data, encoding: .utf8),
              let p = js.withCString({ kou_dispatch(handle, $0) })
        else { return ["ok": false, "error": "dispatch failed"] }
        defer { araware_free_string(p) }
        let s = String(cString: p)
        guard let d = s.data(using: .utf8),
              let r = try? JSONSerialization.jsonObject(with: d) as? [String: Any]
        else { return ["ok": false, "error": "bad response"] }
        return r
    }

    /// Run blocking session calls off the main thread.
    func work<T>(_ f: @escaping (DocSession) -> T) async -> T {
        await withCheckedContinuation { c in
            queue.async { c.resume(returning: f(self)) }
        }
    }

    /// Synchronous session call on the serial queue — for cheap commands
    /// (hit tests) that must answer inside a gesture. Never call from
    /// `queue` itself (deadlock).
    func workSync<T>(_ f: (DocSession) -> T) -> T {
        queue.sync { f(self) }
    }
}
