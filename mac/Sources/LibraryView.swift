import SwiftUI

struct LibraryView: View {
    @EnvironmentObject var store: LibraryStore

    var body: some View {
        NavigationSplitView {
            VStack(spacing: 0) {
                // filter strip
                HStack(spacing: 10) {
                    RatingFilter(rating: $store.minRating)
                    LabelFilter(label: $store.labelFilter)
                    if !store.cameras.isEmpty {
                        FilterMenu(title: "Camera", options: store.cameras, sel: $store.cameraFilter)
                    }
                    if !store.lenses.isEmpty {
                        FilterMenu(title: "Lens", options: store.lenses, sel: $store.lensFilter)
                    }
                    Spacer()
                    SortMenu(sel: $store.sortKey)
                    Text("\(store.filtered.count)/\(store.photos.count)")
                        .font(.system(size: 10).monospacedDigit())
                        .foregroundStyle(Kou.text3)
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
                .background(Kou.bg1)
                .overlay(alignment: .bottom) { Kou.hairline.frame(height: 1) }

                Group {
                    if store.scanning {
                        ProgressView("Scanning…")
                            .tint(Kou.accent)
                            .foregroundStyle(Kou.text2)
                    } else if store.photos.isEmpty {
                        VStack(spacing: 14) {
                            ZStack {
                                Circle().fill(Kou.bg3).frame(width: 72, height: 72)
                                Image(systemName: "photo.on.rectangle.angled")
                                    .font(.system(size: 28))
                                    .foregroundStyle(Kou.accent)
                            }
                            Text("Open a folder of RAW files")
                                .font(.system(size: 12, weight: .medium))
                                .foregroundStyle(Kou.text2)
                            Button("Open Folder…") { store.pickFolder() }
                                .buttonStyle(KouPrimaryButton())
                        }
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                    } else {
                        ScrollView {
                            LazyVGrid(columns: [GridItem(.adaptive(minimum: 148), spacing: 10)], spacing: 10) {
                                ForEach(store.filtered) { photo in
                                    ThumbCell(photo: photo,
                                              selected: store.selection?.path == photo.path,
                                              onRate: { store.setRating(path: photo.path,
                                                                        (photo.rating == $0) ? 0 : $0) })
                                        .onTapGesture { store.selection = photo }
                                }
                            }
                            .padding(10)
                        }
                        .scrollIndicators(.visible)
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            .background(Kou.bg1)
            .navigationTitle(store.folder?.lastPathComponent ?? "koubou")
            .toolbar {
                ToolbarItemGroup {
                    Button { store.pickFolder() } label: {
                        Image(systemName: "folder.badge.plus")
                    }
                    Button { store.refresh() } label: {
                        Image(systemName: "arrow.clockwise")
                    }
                    Button { store.editAsDocument() } label: {
                        Image(systemName: "square.stack.3d.down.forward")
                    }
                    .disabled(store.selection == nil)
                    .help("Edit as layered document")
                }
            }
        } detail: {
            if store.editingDoc {
                DocEditorView(doc: store.doc) { store.editingDoc = false }
            } else if let photo = store.selection {
                EditorView(photo: photo)
                    .id(photo.id)
            } else {
                ZStack {
                    Kou.bg0.ignoresSafeArea()
                    VStack(spacing: 10) {
                        Image(systemName: "camera.aperture")
                            .font(.system(size: 34))
                            .foregroundStyle(Kou.text3)
                        Text("Select a photo")
                            .font(.system(size: 12, weight: .medium))
                            .foregroundStyle(Kou.text3)
                    }
                }
            }
        }
        .preferredColorScheme(.dark)
        .tint(Kou.accent)
    }
}

/// Clickable 0–5 rating filter: tap a star to require that many stars.
struct RatingFilter: View {
    @Binding var rating: Int
    var body: some View {
        HStack(spacing: 2) {
            Image(systemName: "star.fill")
                .font(.system(size: 9))
                .foregroundStyle(Kou.text3)
            ForEach(1...5, id: \.self) { i in
                Image(systemName: i <= rating ? "star.fill" : "star")
                    .font(.system(size: 10))
                    .foregroundStyle(i <= rating ? Kou.gold : Kou.text3)
                    .onTapGesture { rating = (rating == i) ? 0 : i }
            }
            if rating > 0 { Text("+").font(.system(size: 9)).foregroundStyle(Kou.text3) }
        }
    }
}

/// Dropdown text filter for camera/lens metadata.
struct FilterMenu: View {
    let title: String
    let options: [String]
    @Binding var sel: String

    var body: some View {
        Menu {
            Button("All") { sel = "" }
            Divider()
            ForEach(options, id: \.self) { o in
                Button(o) { sel = (sel == o) ? "" : o }
            }
        } label: {
            HStack(spacing: 3) {
                Text(sel.isEmpty ? title : sel)
                    .font(.system(size: 10, weight: sel.isEmpty ? .regular : .semibold))
                    .lineLimit(1)
                    .truncationMode(.middle)
                Image(systemName: "chevron.down")
                    .font(.system(size: 7, weight: .bold))
            }
            .foregroundStyle(sel.isEmpty ? Kou.text3 : Kou.accent)
            .padding(.horizontal, 7)
            .padding(.vertical, 3)
            .background(sel.isEmpty ? Color.clear : Kou.accent.opacity(0.12))
            .clipShape(Capsule())
            .overlay(Capsule().stroke(sel.isEmpty ? Kou.border : Kou.accent.opacity(0.6), lineWidth: 1))
        }
        .menuStyle(.borderlessButton)
        .fixedSize()
    }
}

/// darktable sort dropdown for the grid order.
struct SortMenu: View {
    @Binding var sel: String
    var body: some View {
        Menu {
            ForEach([("name", "Name"), ("date", "Date"), ("rating", "Rating"),
                     ("size", "File Size")], id: \.0) { k, name in
                Button { sel = k } label: {
                    if sel == k { Label(name, systemImage: "checkmark") } else { Text(name) }
                }
            }
        } label: {
            HStack(spacing: 3) {
                Image(systemName: "arrow.up.arrow.down")
                    .font(.system(size: 9, weight: .semibold))
                Text(["date": "Date", "rating": "Rating", "size": "Size"][sel] ?? "Name")
                    .font(.system(size: 10))
            }
            .foregroundStyle(Kou.text3)
            .padding(.horizontal, 7).padding(.vertical, 3)
            .clipShape(Capsule())
            .overlay(Capsule().stroke(Kou.border, lineWidth: 1))
        }
        .menuStyle(.borderlessButton)
        .fixedSize()
        .help("Sort order")
    }
}

/// Label-colour dots used as the library filter.
struct LabelFilter: View {
    @Binding var label: String
    var body: some View {
        HStack(spacing: 5) {
            ForEach(labelColors, id: \.name) { l in
                Circle()
                    .fill(l.color)
                    .frame(width: 10, height: 10)
                    .overlay(Circle().stroke(.white.opacity(0.85), lineWidth: label == l.name ? 1.5 : 0))
                    .opacity(label.isEmpty || label == l.name ? 1 : 0.35)
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

struct ThumbCell: View {
    let photo: Photo
    let selected: Bool
    var onRate: ((Int) -> Void)? = nil
    @State private var image: CGImage?
    @State private var hovering = false

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            ZStack(alignment: .bottom) {
                ZStack {
                    Kou.bg3
                    if let image {
                        Image(image, scale: 1, label: Text(photo.name))
                            .resizable()
                            .scaledToFill()
                    } else {
                        ProgressView().controlSize(.small).tint(Kou.text3)
                    }
                }
                .aspectRatio(1.4, contentMode: .fit)
                .clipped()

                // bottom gradient info bar
                LinearGradient(colors: [.clear, .black.opacity(0.75)],
                               startPoint: .top, endPoint: .bottom)
                    .frame(height: 30)
                HStack(alignment: .lastTextBaseline) {
                    if photo.rating > 0 {
                        Text(String(repeating: "★", count: photo.rating))
                            .font(.system(size: 9))
                            .foregroundStyle(Kou.gold)
                    }
                    Spacer()
                    if photo.has_sidecar {
                        Image(systemName: "checkmark.circle.fill")
                            .font(.system(size: 9))
                            .foregroundStyle(Kou.accent)
                    }
                }
                .padding(.horizontal, 6)
                .padding(.bottom, 5)

                // label dot
                if let c = labelColors.first(where: { $0.name == photo.label })?.color {
                    VStack {
                        HStack {
                            Circle()
                                .fill(c)
                                .frame(width: 8, height: 8)
                                .overlay(Circle().stroke(.black.opacity(0.5), lineWidth: 0.5))
                                .padding(6)
                            Spacer()
                        }
                        Spacer()
                    }
                }

                // darktable lighttable: inline star rating on hover
                if hovering, let onRate {
                    VStack {
                        HStack {
                            Spacer()
                            HStack(spacing: 2) {
                                ForEach(1...5, id: \.self) { i in
                                    Image(systemName: i <= photo.rating ? "star.fill" : "star")
                                        .font(.system(size: 8))
                                        .foregroundStyle(i <= photo.rating ? Kou.gold : .white.opacity(0.75))
                                        .frame(width: 13, height: 16)
                                        .contentShape(Rectangle())
                                        .onTapGesture { onRate(i) }
                                }
                            }
                            .padding(.horizontal, 4).padding(.vertical, 2)
                            .background(.black.opacity(0.55))
                            .clipShape(Capsule())
                            .padding(5)
                        }
                        Spacer()
                    }
                }
            }
            .clipShape(RoundedRectangle(cornerRadius: 7))
            .overlay(
                RoundedRectangle(cornerRadius: 7)
                    .stroke(selected ? Kou.accent : (hovering ? Kou.border : Kou.hairline),
                            lineWidth: selected ? 2 : 1)
            )
            .shadow(color: .black.opacity(selected ? 0.45 : 0.0), radius: 6, y: 2)
            .scaleEffect(hovering ? 1.02 : 1)
            .animation(.easeOut(duration: 0.12), value: hovering)
            .onHover { hovering = $0 }

            Text(photo.name)
                .font(.system(size: 10, weight: selected ? .semibold : .regular))
                .foregroundStyle(selected ? Kou.text1 : Kou.text2)
                .lineLimit(1)
                .truncationMode(.middle)
        }
        .task {
            let img = await KouEngine.shared.work { $0.thumbnail(path: photo.path, maxPx: 400) }
            await MainActor.run { image = img }
        }
    }
}
