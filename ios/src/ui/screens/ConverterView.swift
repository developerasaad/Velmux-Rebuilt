import SwiftUI

private enum ConverterChoice: String, Identifiable {
    case format, videoCodec, resolution, frameRate, audioCodec, sampleRate, channels, precision, bitrate
    var id: String { rawValue }
}

/// Workspace state is transient user intent. Catalogs and applicable choices
/// are cheap Rust queries over the already inspected source.
@MainActor
struct ConverterView: View {
    let seed: WorkspaceSeed
    @ObservedObject var session: AppSession
    @ObservedObject var actions: IosMediaActions
    @Binding var isPresented: Bool
    @State private var configuration: ConversionConfig
    @State private var formats: [FormatOption] = []
    @State private var choices: ConfigurationChoices?
    @State private var choice: ConverterChoice?
    @State private var compatibility: String?
    @State private var edited = false
    @State private var confirmLeave = false
    @State private var submitting = false
    @State private var submissionKey = UUID().uuidString
    @State private var jobsActive = false
    @State private var advanced = false
    @State private var width = ""
    @State private var height = ""
    @State private var numerator = ""
    @State private var denominator = ""
    @State private var fallbackRequired = false
    @State private var fallback: PreviewResult?
    @State private var previewTask: Task<Void, Never>?
    @State private var previewCleanup: Task<Void, Never>?
    @State private var previewVisible = true
    @State private var previewGeneration = UUID()
    @State private var previewBusy = false
    @State private var previewPosition = 0.0

    init(seed: WorkspaceSeed, session: AppSession, actions: IosMediaActions, isPresented: Binding<Bool>) {
        self.seed = seed
        self.session = session
        self.actions = actions
        _isPresented = isPresented
        _configuration = State(initialValue: seed.configuration)
    }

    private var video: Bool { seed.operation == .videoConverter }
    private var hasAudio: Bool { seed.source.info.streams.contains { $0.kind == .audio } }
    private var selectedFormat: FormatOption? { formats.first { $0.format == configuration.format } }
    private var scope: String { "converter-" + seed.source.id }
    private var validNumbers: Bool {
        (configuration.videoSize != .custom || UInt32(width) != nil && UInt32(height) != nil)
        && (configuration.frameRate != .custom || Int32(numerator) != nil && Int32(denominator) != nil)
    }

    var body: some View {
        Form {
            sourceSection
            Section("Output") {
                selection("Format", value: selectedFormat?.label ?? String(describing: configuration.format), field: .format, id: "output-format")
                if video {
                    selection("Codec", value: videoCodecLabel(configuration.videoCodec), field: .videoCodec, id: "video-codec")
                    selection("Resolution", value: sizeLabel(configuration.videoSize), field: .resolution)
                    if configuration.videoSize == .custom {
                        numeric("Width bound", text: $width)
                        numeric("Height bound", text: $height)
                    }
                    selection("Frame rate", value: frameLabel(configuration.frameRate), field: .frameRate)
                    if configuration.frameRate == .custom {
                        numeric("Numerator", text: $numerator)
                        numeric("Denominator", text: $denominator)
                    }
                } else { audioSettings }
                TextField("Filename", text: binding(\.fileName))
                    .textInputAutocapitalization(.never).disableAutocorrection(true)
                    .accessibilityIdentifier("output-filename")
                if let compatibility { Text(compatibility).font(.footnote).foregroundColor(.red).textSelection(.enabled) }
            }.disabled(submitting)
            if video && hasAudio || video && configuration.videoCodec == .h264 {
                Section {
                    DisclosureGroup("Encoding settings", isExpanded: $advanced) {
                        if hasAudio { audioSettings }
                        if configuration.videoCodec == .h264 {
                            VStack(alignment: .leading) {
                                Text("H.264 quality: \(configuration.quality)")
                                Slider(value: Binding(get: { Double(configuration.quality) }, set: { value in change { $0.quality = UInt8(value.rounded()) } }), in: 18...28, step: 1)
                                    .accessibilityLabel("H.264 quality")
                                Text("Lower values retain more detail and use more space.").font(.footnote).foregroundColor(.secondary)
                            }
                        }
                    }
                }.disabled(submitting)
            }
            Section {
                Button(action: submit) { HStack { Spacer(); if submitting { ProgressView() }; Text(submitting ? "Submitting" : "Convert"); Spacer() } }
                    .disabled(submitting || !validNumbers || compatibility != nil)
                    .accessibilityIdentifier("convert")
            }
        }
        .navigationTitle(video ? "Video Converter" : "Audio Converter")
        .navigationBarTitleDisplayMode(.inline)
        .navigationBarBackButtonHidden(true)
        .toolbar {
            ToolbarItem(placement: .navigationBarLeading) {
                Button { if edited { confirmLeave = true } else { leave() } } label: { Label("Back", systemImage: "chevron.left") }.disabled(submitting)
            }
            ToolbarItem(placement: .navigationBarTrailing) {
                NavigationLink(isActive: $jobsActive) { JobsView(session: session, actions: actions) } label: { Text("Jobs") }.accessibilityIdentifier("jobs-action")
            }
        }
        .sheet(item: $choice) { field in choiceSheet(field) }
        .confirmationDialog("Discard workspace changes?", isPresented: $confirmLeave, titleVisibility: .visible) {
            Button("Discard changes", role: .destructive, action: leave)
            Button("Keep editing", role: .cancel) {}
        }
        .onAppear { previewVisible = true; session.setPresentationScope(scope); refreshChoices() }
        .onDisappear { previewVisible = false; stopPreview() }
        .onChange(of: width) { _ in commitNumbers() }
        .onChange(of: height) { _ in commitNumbers() }
        .onChange(of: numerator) { _ in commitNumbers() }
        .onChange(of: denominator) { _ in commitNumbers() }
    }

    private var sourceSection: some View {
        Section {
            if previewVisible && (!fallbackRequired || fallback != nil) {
                MediaPreview(locator: fallback?.locator ?? seed.source.locator, actions: actions,
                             onUnsupported: { fallbackRequired = true }, handle: seed.previewHandle)
                    .id(fallback?.locator ?? seed.source.locator)
                    .frame(height: video ? 200 : 120).accessibilityIdentifier("media-preview")
            }
            Text(seed.source.displayName).font(.headline).lineLimit(2).accessibilityIdentifier("source-name")
            Text(sourceSummary).font(.footnote).foregroundColor(.secondary).textSelection(.enabled)
            if fallbackRequired {
                if let fallback { Text("Compatibility preview: \(duration(fallback.startUs))–\(duration(fallback.startUs + fallback.durationUs))").font(.footnote).foregroundColor(.secondary) }
                if let length = seed.source.info.durationUs, length > 10_000_000 {
                    Slider(value: $previewPosition, in: 0...Double(max(0, length - 1_000_000)) / 1_000_000)
                        .accessibilityLabel("Compatibility preview start, seconds")
                }
                HStack {
                    if previewBusy { ProgressView(); Button("Cancel preview") { stopPreview() } }
                    else { Button(fallback == nil ? "Prepare compatibility preview" : "Preview selected segment", action: preparePreview) }
                }
            }
        }
    }

    @ViewBuilder private var audioSettings: some View {
        selection(video ? "Audio codec" : "Codec", value: audioCodecLabel(configuration.audioCodec), field: .audioCodec, id: "audio-codec")
        selection("Sample rate", value: configuration.sampleRate.map { "\($0) Hz" } ?? "Original", field: .sampleRate)
        selection("Channels", value: layoutLabel(configuration.channelLayout ?? .original), field: .channels)
        if !(choices?.audioPrecisions.isEmpty ?? true) {
            selection("Precision", value: precisionLabel(configuration.audioPrecision ?? .original), field: .precision)
        }
        if !(choices?.audioBitrates.isEmpty ?? true) {
            selection("Audio bitrate", value: configuration.audioBitrate.map { "\($0 / 1000) kbps" } ?? "Automatic", field: .bitrate)
        }
    }

    private func selection(_ title: String, value: String, field: ConverterChoice, id: String = "") -> some View {
        Button { choice = field } label: {
            HStack { Text(title); Spacer(); Text(value).foregroundColor(.secondary); Image(systemName: "chevron.down").font(.caption).foregroundColor(.secondary) }
        }.foregroundColor(.primary).accessibilityIdentifier(id)
    }

    private func numeric(_ title: String, text: Binding<String>) -> some View {
        HStack { Text(title); TextField(title, text: text).keyboardType(.numberPad).multilineTextAlignment(.trailing) }
    }

    @ViewBuilder private func choiceSheet(_ field: ConverterChoice) -> some View {
        switch field {
        case .format:
            NativeChoiceSheet(title: "Format", values: formats.map(\.format), selected: configuration.format,
                              label: { format in formats.first { $0.format == format }?.label ?? String(describing: format) }) { value in change { $0.format = value }; choice = nil }
        case .videoCodec:
            NativeChoiceSheet(title: "Video codec", values: selectedFormat?.videoCodecs ?? [], selected: configuration.videoCodec, label: videoCodecLabel) { value in change { $0.videoCodec = value }; choice = nil }
        case .resolution:
            NativeChoiceSheet(title: "Resolution", values: choices?.videoSizes ?? [], selected: configuration.videoSize, label: sizeLabel) { value in
                if value == .custom, let choices {
                    width = String(configuration.customWidth ?? choices.defaultCustomWidth)
                    height = String(configuration.customHeight ?? choices.defaultCustomHeight)
                }
                change { $0.videoSize = value }; commitNumbers(); choice = nil
            }
        case .frameRate:
            NativeChoiceSheet(title: "Frame rate", values: choices?.frameRates ?? [], selected: configuration.frameRate, label: frameLabel) { value in
                if value == .custom, let choices {
                    numerator = String(configuration.customFrameRate?.numerator ?? choices.defaultCustomFrameRate.numerator)
                    denominator = String(configuration.customFrameRate?.denominator ?? choices.defaultCustomFrameRate.denominator)
                }
                change { $0.frameRate = value }; commitNumbers(); choice = nil
            }
        case .audioCodec:
            NativeChoiceSheet(title: "Audio codec", values: selectedFormat?.audioCodecs ?? [], selected: configuration.audioCodec, label: audioCodecLabel) { value in change { $0.audioCodec = value }; choice = nil }
        case .sampleRate:
            NativeChoiceSheet(title: "Sample rate", values: choices?.sampleRates ?? [], selected: configuration.sampleRate ?? 0, label: { $0 == 0 ? "Original" : "\($0) Hz" }) { value in change { $0.sampleRate = value == 0 ? nil : value }; choice = nil }
        case .channels:
            NativeChoiceSheet(title: "Channels", values: choices?.channelLayouts ?? [], selected: configuration.channelLayout ?? .original, label: layoutLabel) { value in change { $0.channelLayout = value; $0.channels = nil }; choice = nil }
        case .precision:
            NativeChoiceSheet(title: "Sample precision", values: choices?.audioPrecisions ?? [], selected: configuration.audioPrecision ?? .original, label: precisionLabel) { value in change { $0.audioPrecision = value }; choice = nil }
        case .bitrate:
            NativeChoiceSheet(title: "Audio bitrate", values: choices?.audioBitrates ?? [], selected: configuration.audioBitrate ?? 0, label: { $0 == 0 ? "Automatic" : "\(Double($0) / 1000) kbps" }) { value in change { $0.audioBitrate = value == 0 ? nil : value }; choice = nil }
        }
    }

    private func binding<Value>(_ path: WritableKeyPath<ConversionConfig, Value>) -> Binding<Value> {
        Binding(get: { configuration[keyPath: path] }, set: { value in
            configuration[keyPath: path] = value; edited = true; submissionKey = UUID().uuidString
            // Filename editing has no compatibility or planning cost.
        })
    }
    private func change(_ edit: (inout ConversionConfig) -> Void) {
        edit(&configuration); edited = true; submissionKey = UUID().uuidString; refreshChoices()
    }
    private func commitNumbers() {
        guard configuration.videoSize == .custom || configuration.frameRate == .custom else { return }
        change { configuration in
            if configuration.videoSize == .custom { configuration.customWidth = UInt32(width); configuration.customHeight = UInt32(height) }
            if configuration.frameRate == .custom {
                configuration.customFrameRate = Int32(numerator).flatMap { n in Int32(denominator).map { Rational(numerator: n, denominator: $0) } }
            }
        }
    }
    private func refreshChoices() {
        do {
            formats = try session.catalog(sourceId: seed.source.id, operation: seed.operation)
            choices = try session.choices(sourceId: seed.source.id, configuration: configuration)
            compatibility = nil
            // Explicit choices remain intact when another setting excludes them.
            if let format = selectedFormat,
               video && !format.videoCodecs.contains(configuration.videoCodec) || hasAudio && !format.audioCodecs.contains(configuration.audioCodec) {
                compatibility = "The selected codec is incompatible with this format. Choose a compatible codec or another format."
            }
        } catch { compatibility = session.errorDetail(error) }
    }
    private func submit() {
        guard !submitting, validNumbers else { return }
        submitting = true
        let acceptedConfiguration = configuration
        let key = submissionKey
        Task {
            defer { submitting = false }
            do {
                _ = try await session.submit(sourceId: seed.source.id, key: key, configuration: acceptedConfiguration)
                edited = false; submissionKey = UUID().uuidString; stopPreview(); jobsActive = true
            } catch { session.present(error: error, scope: scope, title: "Unable to convert") }
        }
    }
    private func preparePreview() {
        stopPreview()
        let generation = UUID(); previewGeneration = generation; previewBusy = true
        let cleanup = previewCleanup
        previewTask = Task {
            // A retired player's artifact must finish releasing before another
            // preview for this inspection can be prepared.
            await cleanup?.value
            guard generation == previewGeneration, !Task.isCancelled else { return }
            do {
                let result = try await session.preparePreview(sourceId: seed.source.id, startUs: Int64(previewPosition * 1_000_000))
                guard generation == previewGeneration, !Task.isCancelled else { return }
                fallback = result
            } catch { if generation == previewGeneration { session.present(error: error, scope: scope, title: "Unable to preview") } }
            if generation == previewGeneration { previewBusy = false; previewTask = nil }
        }
    }
    private func stopPreview() {
        previewGeneration = UUID(); previewTask?.cancel(); previewTask = nil; previewBusy = false
        seed.previewHandle.release(); fallback = nil
        let previous = previewCleanup
        previewCleanup = Task {
            await previous?.value
            try? await session.releasePreview(sourceId: seed.source.id)
        }
    }
    private func leave() { stopPreview(); isPresented = false }
    private var sourceSummary: String {
        var parts = [seed.source.info.container]
        if let size = seed.source.info.byteSize { parts.append(nativeBytes(size)) }
        if let time = seed.source.info.durationUs { parts.append(duration(time)) }
        for stream in seed.source.info.streams where stream.kind == .video || stream.kind == .audio {
            var details = stream.codec
            if let video = stream.video, let width = video.width, let height = video.height { details += " \(width)×\(height)" }
            if let audio = stream.audio { if let rate = audio.sampleRate { details += " \(rate) Hz" }; if let channels = audio.channels { details += " · \(channels) ch" } }
            parts.append(details)
        }
        return parts.joined(separator: " · ")
    }
    private func duration(_ microseconds: Int64) -> String {
        let seconds = max(0, microseconds / 1_000_000)
        return String(format: "%ld:%02ld", seconds / 60, seconds % 60)
    }
}

private struct NativeChoiceSheet<Value: Hashable>: View {
    let title: String
    let values: [Value]
    let selected: Value
    let label: (Value) -> String
    let onSelect: (Value) -> Void
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        NavigationView {
            List {
                ForEach(values, id: \.self) { value in
                    Button { onSelect(value) } label: {
                        HStack { Text(label(value)); Spacer(); if value == selected { Image(systemName: "checkmark").accessibilityLabel("Selected") } }
                    }.foregroundColor(.primary)
                }
                if values.isEmpty { Text("No applicable choices for the current settings.").foregroundColor(.secondary) }
            }.navigationTitle(title).navigationBarTitleDisplayMode(.inline)
                .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Done") { dismiss() } } }
        }.navigationViewStyle(.stack)
    }
}

private func videoCodecLabel(_ value: VideoCodec) -> String {
    switch value {
    case .automatic: return "Automatic"; case .copy: return "Copy source"; case .h264: return "H.264"; case .hevc: return "HEVC"; case .av1: return "AV1"
    case .vp8: return "VP8"; case .vp9: return "VP9"; case .mpeg4: return "MPEG-4 Part 2"; case .ffv1: return "FFV1"; case .mjpeg: return "MJPEG"
    }
}
private func audioCodecLabel(_ value: AudioCodec) -> String {
    switch value {
    case .automatic: return "Automatic"; case .copy: return "Copy source"; case .aac: return "AAC-LC"; case .mp3: return "MP3"; case .pcmS16le: return "PCM 16-bit"; case .pcm: return "PCM"
    case .flac: return "FLAC"; case .alac: return "ALAC"; case .opus: return "Opus"; case .vorbis: return "Vorbis"; case .ac3: return "AC-3"; case .eac3: return "E-AC-3"
    case .wavpack: return "WavPack"; case .wma: return "WMA"; case .heAac: return "HE-AAC"; case .amrNb: return "AMR-NB"; case .amrWb: return "AMR-WB"
    }
}
private func sizeLabel(_ value: VideoSize) -> String {
    switch value { case .original: return "Original"; case .p2160: return "2160p"; case .p1440: return "1440p"; case .p1080: return "1080p"; case .hd720: return "720p"; case .sd480: return "480p"; case .p360: return "360p"; case .custom: return "Custom" }
}
private func frameLabel(_ value: FrameRate) -> String {
    switch value {
    case .original: return "Original"; case .fps23976: return "23.976"; case .fps24: return "24"; case .fps25: return "25"; case .fps2997: return "29.97"; case .fps30: return "30"
    case .fps48: return "48"; case .fps50: return "50"; case .fps5994: return "59.94"; case .fps60: return "60"; case .custom: return "Custom"
    }
}
private func layoutLabel(_ value: AudioLayout) -> String {
    switch value { case .original: return "Original"; case .mono: return "Mono"; case .stereo: return "Stereo"; case .surround51: return "5.1 rear"; case .surround51Side: return "5.1 side"; case .surround71: return "7.1" }
}
private func precisionLabel(_ value: AudioPrecision) -> String {
    switch value { case .original: return "Original"; case .pcm8: return "8-bit integer"; case .pcm16: return "16-bit integer"; case .pcm24: return "24-bit integer"; case .pcm32: return "32-bit integer"; case .float32: return "32-bit float"; case .float64: return "64-bit float" }
}
