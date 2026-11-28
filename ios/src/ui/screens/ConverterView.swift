import SwiftUI

private enum ConverterChoice: String, Identifiable {
    case format, videoCodec, resolution, frameRate, audioCodec, sampleRate, channels, precision, rotation
    var id: String { rawValue }
}

private enum ConverterSheet: Identifiable {
    case output(ConverterChoice)
    case compression(CompressionField)

    var id: String {
        switch self {
        case .output(let field): return "output-" + field.id
        case .compression(let field): return "compression-" + field.id
        }
    }
}

/// Workspace state is transient user intent. Catalogs and applicable choices
/// are cheap Rust queries over the already inspected source.
@MainActor
struct ConverterView: View {
    let seed: WorkspaceSeed
    private let adjustmentDefaults: AudioAdjustments
    private let videoAdjustmentDefaults: VideoAdjustments
    @ObservedObject var session: AppSession
    @ObservedObject var actions: IosMediaActions
    @Binding var isPresented: Bool
    @State private var configuration: ConversionConfig
    @State private var formats: [FormatOption] = []
    @State private var choices: ConfigurationChoices?
    @State private var choice: ConverterChoice?
    @State private var compressionChoice: CompressionField?
    @State private var compatibility: String?
    @State private var edited = false
    @State private var confirmLeave = false
    @State private var submitting = false
    @State private var submissionKey = UUID().uuidString
    @State private var jobsActive = false
    @State private var advanced = false
    @State private var compressionDraft: CompressionDraft
    @State private var compressionError: String?
    @State private var outputEstimate: OutputEstimate?
    @State private var estimateIssue: String?
    @State private var estimatedConfiguration: ConversionConfig?
    @State private var estimatedDraft: CompressionDraft?
    @State private var adjustmentsExpanded = false
    @State private var gainText: String
    @State private var fadeInText: String
    @State private var fadeOutText: String
    @State private var speedText: String
    @State private var preservePitch: Bool
    @State private var normalizeLoudness: Bool
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
    @FocusState private var focusedField: String?

    init(seed: WorkspaceSeed, session: AppSession, actions: IosMediaActions, isPresented: Binding<Bool>) {
        self.seed = seed
        self.session = session
        self.actions = actions
        _isPresented = isPresented
        _configuration = State(initialValue: seed.configuration)
        _compressionDraft = State(initialValue: CompressionDraft(seed.configuration.compression))
        let defaults = session.audioAdjustmentDefaults()
        adjustmentDefaults = defaults
        videoAdjustmentDefaults = session.videoAdjustmentDefaults()
        let adjustments = seed.configuration.audioAdjustments ?? defaults
        _gainText = State(initialValue: String(Double(adjustments.gainCentidb) / 100))
        _fadeInText = State(initialValue: String(Double(adjustments.fadeInUs) / 1_000_000))
        _fadeOutText = State(initialValue: String(Double(adjustments.fadeOutUs) / 1_000_000))
        _speedText = State(initialValue: String(format: "%.3f", Double(adjustments.speedMilli) / 1000))
        _preservePitch = State(initialValue: adjustments.preservePitch)
        _normalizeLoudness = State(initialValue: adjustments.normalizeLoudness)
    }

    private var video: Bool { seed.operation == .videoConverter }
    private var hasAudio: Bool { seed.source.info.streams.contains { $0.kind == .audio } }
    private var selectedFormat: FormatOption? { formats.first { $0.format == configuration.format } }
    private var scope: String { "converter-" + seed.source.id }
    private var validNumbers: Bool {
        (configuration.videoSize != .custom || UInt32(width) != nil && UInt32(height) != nil)
        && (configuration.frameRate != .custom || Int32(numerator) != nil && Int32(denominator) != nil)
        && (video || parsedAdjustments != nil)
        && compressionError == nil
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
                    .focused($focusedField, equals: "filename").submitLabel(.done)
                    .onSubmit { focusedField = nil }
                    .accessibilityIdentifier("output-filename")
                if let compatibility { Text(compatibility).font(.footnote).foregroundColor(.red).textSelection(.enabled) }
            }.disabled(submitting)
            if video && hasAudio {
                Section {
                    DisclosureGroup("Audio settings", isExpanded: $advanced) { audioSettings }
                }.disabled(submitting)
            }
            compressionControls.disabled(submitting)
            if video, let limits = choices?.videoAdjustmentLimits {
                Section {
                    DisclosureGroup("Output adjustments", isExpanded: $adjustmentsExpanded) {
                        selection("Rotation", value: rotationLabel(currentVideoAdjustments.rotation), field: .rotation, id: "video-rotation")
                            .disabled(limits.rotations.isEmpty)
                        Toggle("Flip horizontally", isOn: videoAdjustmentBinding(\.horizontalFlip))
                            .disabled(!limits.canFlipHorizontal && !currentVideoAdjustments.horizontalFlip)
                            .accessibilityIdentifier("video-flip-horizontal")
                        Toggle("Flip vertically", isOn: videoAdjustmentBinding(\.verticalFlip))
                            .disabled(!limits.canFlipVertical && !currentVideoAdjustments.verticalFlip)
                            .accessibilityIdentifier("video-flip-vertical")
                        Text("Rotation is clockwise from the source's displayed orientation. Flips apply afterward. The preview shows the source.")
                            .font(.footnote).foregroundColor(.secondary)
                    }
                }.disabled(submitting)
            }
            if !video, let limits = choices?.audioAdjustmentLimits {
                Section {
                    DisclosureGroup("Audio adjustments", isExpanded: $adjustmentsExpanded) {
                        if let policy = limits.loudnessNormalization {
                            Toggle("Normalize loudness", isOn: $normalizeLoudness)
                                .accessibilityIdentifier("audio-normalize-loudness")
                            if normalizeLoudness {
                                Text("Targets \(String(format: "%.1f", Double(policy.targetCentiLufs) / 100)) LUFS and \(String(format: "%.1f", Double(policy.maximumTruePeakCentidb) / 100)) dBTP before gain and fades. Results vary with the audio and encoding.")
                                    .font(.footnote).foregroundColor(.secondary)
                            }
                        }
                        HStack {
                            Text("Gain")
                            TextField("dB", text: $gainText).keyboardType(.numbersAndPunctuation)
                                .multilineTextAlignment(.trailing).focused($focusedField, equals: "gain")
                                .accessibilityIdentifier("audio-gain")
                            Text("dB").foregroundColor(.secondary)
                        }
                        Slider(value: Binding(get: { Double(configuration.audioAdjustments?.gainCentidb ?? adjustmentDefaults.gainCentidb) }, set: { value in
                            gainText = String(format: "%.2f", value.rounded() / 100)
                        }), in: Double(limits.minimumGainCentidb)...Double(limits.maximumGainCentidb), step: 10)
                            .accessibilityLabel("Audio gain")
                        HStack {
                            Text("Playback speed")
                            TextField("Multiplier", text: $speedText).keyboardType(.decimalPad)
                                .multilineTextAlignment(.trailing).focused($focusedField, equals: "audio-speed")
                                .accessibilityIdentifier("audio-speed")
                            Text("×").foregroundColor(.secondary)
                        }
                        Slider(value: Binding(get: {
                            Double(configuration.audioAdjustments?.speedMilli ?? adjustmentDefaults.speedMilli)
                        }, set: { value in speedText = String(format: "%.3f", value.rounded() / 1000) }),
                               in: Double(limits.minimumSpeedMilli)...Double(limits.maximumSpeedMilli), step: 1)
                            .accessibilityLabel("Audio playback speed")
                            .accessibilityIdentifier("audio-speed-slider")
                        Text("\(String(format: "%.3g", Double(limits.minimumSpeedMilli) / 1000))×–\(String(format: "%.3g", Double(limits.maximumSpeedMilli) / 1000))× · up to three decimal places")
                            .font(.footnote).foregroundColor(.secondary)
                        Toggle("Preserve pitch", isOn: $preservePitch)
                            .disabled(parsedSpeed == nil || parsedSpeed == adjustmentDefaults.speedMilli)
                            .accessibilityIdentifier("audio-preserve-pitch")
                        if limits.maximumFadeUs != nil {
                            decimalSeconds("Fade in", text: $fadeInText)
                            decimalSeconds("Fade out", text: $fadeOutText)
                        }
                        Text("Speed changes output duration. Fades use output seconds after the speed change. Positive gain includes peak protection.")
                            .font(.footnote).foregroundColor(.secondary)
                        if parsedAdjustments == nil { Text("Enter a signed gain, nonnegative fade durations and a playback speed within the displayed range, using at most three decimal places.").font(.footnote).foregroundColor(.red) }
                    }
                }.disabled(submitting)
            }
            estimateSection
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
            ToolbarItemGroup(placement: .keyboard) {
                Spacer()
                Button("Done") { focusedField = nil }
            }
        }
        .sheet(item: sheetBinding) { sheet in
            switch sheet {
            case .output(let field): choiceSheet(field)
            case .compression(let field): compressionControls.sheet(field)
            }
        }
        .confirmationDialog("Discard workspace changes?", isPresented: $confirmLeave, titleVisibility: .visible) {
            Button("Discard changes", role: .destructive, action: leave)
            Button("Keep editing", role: .cancel) {}
        }
        .onAppear { previewVisible = true; session.setPresentationScope(scope); refreshChoices(); commitCompression(refresh: false) }
        .onDisappear { previewVisible = false; stopPreview() }
        .onChange(of: compressionDraft) { _ in
            edited = true; submissionKey = UUID().uuidString; commitCompression()
        }
        .onChange(of: width) { _ in commitNumbers() }
        .onChange(of: height) { _ in commitNumbers() }
        .onChange(of: numerator) { _ in commitNumbers() }
        .onChange(of: denominator) { _ in commitNumbers() }
        .onChange(of: gainText) { _ in commitAdjustments() }
        .onChange(of: fadeInText) { _ in commitAdjustments() }
        .onChange(of: fadeOutText) { _ in commitAdjustments() }
        .onChange(of: speedText) { _ in commitAdjustments() }
        .onChange(of: preservePitch) { _ in commitAdjustments() }
        .onChange(of: normalizeLoudness) { _ in commitAdjustments() }
    }

    private var compressionControls: CompressionControls {
        CompressionControls(draft: $compressionDraft,
                            choices: choices?.compressionChoices,
                            video: video, hasAudio: hasAudio,
                            error: compressionError, focus: $focusedField,
                            selection: $compressionChoice)
    }

    // A Section inside Form becomes multiple native cells. Present choices
    // from the stable converter host rather than those transient cell hosts.
    private var sheetBinding: Binding<ConverterSheet?> {
        Binding(get: {
            if let choice { return .output(choice) }
            return compressionChoice.map { .compression($0) }
        }, set: { value in
            switch value {
            case .output(let field): choice = field; compressionChoice = nil
            case .compression(let field): compressionChoice = field; choice = nil
            case nil: choice = nil; compressionChoice = nil
            }
        })
    }

    private var sourceSection: some View {
        Section {
            if previewVisible && (!fallbackRequired || fallback != nil) {
                Text("Source preview").font(.caption).foregroundColor(.secondary)
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

    private var estimateCacheKey: ConversionConfig {
        var key = configuration
        // Filename is ignored only in this presentation cache key. The core
        // always receives actual user intent and owns estimate validation.
        key.fileName = ""
        return key
    }

    private var estimateIsCurrent: Bool {
        validNumbers && compatibility == nil
            && estimatedConfiguration == estimateCacheKey
            && estimatedDraft == compressionDraft
    }

    private var estimateSection: some View {
        Section("Output size") {
            if estimateIsCurrent, let estimate = outputEstimate {
                switch estimate.size {
                case .sourceSizeReference(let bytes):
                    LabeledContentRow(title: "Source-size reference", value: nativeBytes(bytes))
                        .accessibilityIdentifier("output-estimate")
                case .mediaPayloadApproximation(let bytes, _):
                    LabeledContentRow(title: "Approximate media payload", value: nativeBytes(bytes))
                        .accessibilityIdentifier("output-estimate")
                case .unavailable:
                    Text("Estimate unavailable").foregroundColor(.secondary)
                        .accessibilityIdentifier("output-estimate")
                }
                Text(estimate.explanation).font(.footnote).foregroundColor(.secondary)
                    .accessibilityIdentifier("estimate-explanation")
                if let bytes = estimate.requestedTargetBytes {
                    LabeledContentRow(title: "Requested target", value: nativeBytes(bytes))
                        .accessibilityIdentifier("requested-target-size")
                }
            } else if estimateIsCurrent, let estimateIssue {
                Text("Estimate unavailable").foregroundColor(.secondary)
                Text(estimateIssue).font(.footnote).foregroundColor(.secondary)
            } else {
                Text("Resolve the current settings to show an output-size reference or estimate.")
                    .font(.footnote).foregroundColor(.secondary)
            }
        }
    }

    private func refreshEstimate() {
        guard validNumbers, compatibility == nil else {
            outputEstimate = nil; estimateIssue = nil
            estimatedConfiguration = nil; estimatedDraft = nil
            return
        }
        let key = estimateCacheKey
        guard estimatedConfiguration != key || estimatedDraft != compressionDraft else { return }
        outputEstimate = nil; estimateIssue = nil
        estimatedConfiguration = key; estimatedDraft = compressionDraft
        do { outputEstimate = try session.estimate(sourceId: seed.source.id, configuration: configuration) }
        catch { estimateIssue = session.errorDetail(error) }
    }

    @ViewBuilder private var audioSettings: some View {
        selection(video ? "Audio codec" : "Codec", value: audioCodecLabel(configuration.audioCodec), field: .audioCodec, id: "audio-codec")
        selection("Sample rate", value: configuration.sampleRate.map { "\($0) Hz" } ?? "Original", field: .sampleRate)
        selection("Channels", value: layoutLabel(configuration.channelLayout ?? .original), field: .channels)
        if !(choices?.audioPrecisions.isEmpty ?? true) {
            selection("Precision", value: precisionLabel(configuration.audioPrecision ?? .original), field: .precision)
        }
    }

    private func selection(_ title: String, value: String, field: ConverterChoice, id: String = "") -> some View {
        Button { choice = field } label: {
            HStack { Text(title); Spacer(); Text(value).foregroundColor(.secondary); Image(systemName: "chevron.down").font(.caption).foregroundColor(.secondary) }
        }.foregroundColor(.primary).accessibilityIdentifier(id)
    }

    private func numeric(_ title: String, text: Binding<String>) -> some View {
        HStack { Text(title); TextField(title, text: text).keyboardType(.numberPad).multilineTextAlignment(.trailing).focused($focusedField, equals: title) }
    }

    private func decimalSeconds(_ title: String, text: Binding<String>) -> some View {
        HStack { Text(title); TextField("Seconds", text: text).keyboardType(.decimalPad).multilineTextAlignment(.trailing).focused($focusedField, equals: title); Text("s").foregroundColor(.secondary) }
    }

    private var parsedSpeed: UInt32? {
        guard let speed = decimalValue(speedText, precision: 3, signed: false),
              let milli = UInt32(exactly: (speed * 1000).rounded()) else { return nil }
        if let limits = choices?.audioAdjustmentLimits,
           !(limits.minimumSpeedMilli...limits.maximumSpeedMilli).contains(milli) { return nil }
        return milli
    }

    private var parsedAdjustments: AudioAdjustments? {
        guard let gain = decimalValue(gainText, precision: 2, signed: true),
              let centidb = Int32(exactly: (gain * 100).rounded()),
              let fadeIn = decimalValue(fadeInText, precision: 6, signed: false),
              let inUs = UInt64(exactly: (fadeIn * 1_000_000).rounded()),
              let fadeOut = decimalValue(fadeOutText, precision: 6, signed: false),
              let outUs = UInt64(exactly: (fadeOut * 1_000_000).rounded()),
              let speed = parsedSpeed else { return nil }
        var adjustments = configuration.audioAdjustments ?? adjustmentDefaults
        adjustments.gainCentidb = centidb
        adjustments.fadeInUs = inUs
        adjustments.fadeOutUs = outUs
        adjustments.speedMilli = speed
        adjustments.preservePitch = preservePitch
        adjustments.normalizeLoudness = normalizeLoudness
        return adjustments
    }

    private func decimalValue(_ text: String, precision: Int, signed: Bool) -> Double? {
        let pattern = (signed ? "^[+-]?" : "^") + "[0-9]+(?:\\.[0-9]{1,\(precision)})?$"
        guard text.count <= 24, text.range(of: pattern, options: .regularExpression) != nil,
              let value = Double(text), value.isFinite else { return nil }
        return value
    }

    private func commitAdjustments() {
        guard !video, let parsedAdjustments else { return }
        change { $0.audioAdjustments = parsedAdjustments }
    }

    private var currentVideoAdjustments: VideoAdjustments {
        configuration.videoAdjustments ?? videoAdjustmentDefaults
    }

    private func videoAdjustmentBinding(_ path: WritableKeyPath<VideoAdjustments, Bool>) -> Binding<Bool> {
        Binding(get: { currentVideoAdjustments[keyPath: path] }, set: { value in
            var adjustments = currentVideoAdjustments
            adjustments[keyPath: path] = value
            change { $0.videoAdjustments = adjustments }
        })
    }

    private func rotationLabel(_ value: VideoRotation) -> String {
        switch value {
        case .zero: return "0°"
        case .clockwise90: return "90°"
        case .clockwise180: return "180°"
        case .clockwise270: return "270°"
        }
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
        case .rotation:
            NativeChoiceSheet(title: "Clockwise rotation", values: choices?.videoAdjustmentLimits?.rotations ?? [], selected: currentVideoAdjustments.rotation, label: rotationLabel) { value in
                var adjustments = currentVideoAdjustments
                adjustments.rotation = value
                change { $0.videoAdjustments = adjustments }; choice = nil
            }
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
        commitCompression(refresh: false)
    }
    private func commitCompression(refresh: Bool = true) {
        do {
            let settings = try compressionDraft.settings(session: session, choices: choices?.compressionChoices, video: video)
            configuration.compression = settings
            configuration.quality = 23
            configuration.audioBitrate = nil
            compressionError = nil
            if refresh { refreshChoices() }
            refreshEstimate()
        } catch {
            outputEstimate = nil; estimateIssue = nil
            estimatedConfiguration = nil; estimatedDraft = nil
            // Retain the last representable configuration, but never submit it
            // while the visible active draft is invalid or incompatible.
            compressionError = session.errorDetail(error)
        }
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
        // Read the visible draft synchronously as well as on field changes.
        // A keyboard edit must never submit a previous valid compression value.
        if !video, let parsedAdjustments { configuration.audioAdjustments = parsedAdjustments }
        commitCompression(refresh: false)
        guard !submitting, validNumbers, compatibility == nil else { return }
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

/// Inactive draft fields remain local to the workspace. Only the active mode's
/// typed fields enter an immutable Rust request; codec rules come from Rust.
private struct CompressionDraft: Equatable {
    var enabled = false
    var mode = CompressionMode.highQuality
    var optimizeResolution = false
    var optimizeFrameRate = false
    var targetText = ""
    var targetUnit = TargetSizeUnit.megabytes
    var videoKind: CompressionControlKind?
    var videoValue = ""
    var audioKind: CompressionControlKind?
    var audioValue = ""

    init(_ settings: CompressionSettings?) {
        guard let settings else { return }
        enabled = settings.reduceFileSize
        mode = settings.mode
        optimizeResolution = settings.optimizeResolution
        optimizeFrameRate = settings.optimizeFrameRate
        if let target = settings.target {
            targetUnit = target.unit
            let whole = target.amountMilli / 1000
            let remainder = target.amountMilli % 1000
            targetText = String(whole)
            if remainder != 0 {
                targetText += "." + String(format: "%03llu", remainder)
            }
        }
        if let control = settings.customVideo {
            let pair: (CompressionControlKind, UInt64)
            switch control {
            case .averageBitrate(let value): pair = (.videoAverageBitrate, value)
            case .h264Crf(let value): pair = (.h264Crf, UInt64(value))
            case .hevcCrf(let value): pair = (.hevcCrf, UInt64(value))
            case .av1Crf(let value): pair = (.av1Crf, UInt64(value))
            case .vp8Cq(let value): pair = (.vp8Cq, UInt64(value))
            case .vp9Crf(let value): pair = (.vp9Crf, UInt64(value))
            case .mpeg4Qscale(let value): pair = (.mpeg4Qscale, UInt64(value))
            case .mjpegQscale(let value): pair = (.mjpegQscale, UInt64(value))
            case .ffv1Context(let large): pair = (.ffv1Context, large ? 1 : 0)
            }
            videoKind = pair.0; videoValue = String(pair.1)
        }
        if let control = settings.customAudio {
            let pair: (CompressionControlKind, UInt64)
            switch control {
            case .averageBitrate(let value): pair = (.audioAverageBitrate, UInt64(value))
            case .mp3VbrQuality(let value): pair = (.mp3VbrQuality, UInt64(value))
            case .vorbisQuality(let value): pair = (.vorbisQuality, UInt64(value))
            case .flacLevel(let value): pair = (.flacLevel, UInt64(value))
            case .alacLevel(let value): pair = (.alacLevel, UInt64(value))
            case .wavpackLevel(let value): pair = (.wavpackLevel, UInt64(value))
            }
            audioKind = pair.0; audioValue = String(pair.1)
        }
    }

    @MainActor
    func settings(session: AppSession, choices: CompressionChoices?, video: Bool) throws -> CompressionSettings {
        var target: TargetSize?
        var videoControl: VideoRateControl?
        var audioControl: AudioRateControl?
        if enabled {
            switch mode {
            case .targetSize:
                target = try session.parseTarget(value: targetText, unit: targetUnit)
            case .custom:
                if video, let kind = videoKind {
                    let value = try checkedValue(videoValue, kind: kind, choices: choices?.video ?? [])
                    videoControl = try session.videoCompressionControl(kind: kind, value: value)
                }
                if let kind = audioKind {
                    let value = try checkedValue(audioValue, kind: kind, choices: choices?.audio ?? [])
                    audioControl = try session.audioCompressionControl(kind: kind, value: value)
                }
                guard videoControl != nil || audioControl != nil else {
                    throw NativePresentationError("Choose an applicable Custom control for video or audio.")
                }
            default: break
            }
        }
        return CompressionSettings(reduceFileSize: enabled, mode: mode,
                                   optimizeResolution: enabled && video && optimizeResolution,
                                   optimizeFrameRate: enabled && video && optimizeFrameRate,
                                   target: target, customVideo: videoControl, customAudio: audioControl)
    }

    private func checkedValue(_ text: String, kind: CompressionControlKind, choices: [CompressionControlOption]) throws -> UInt64 {
        guard let option = choices.first(where: { $0.kind == kind }) else {
            throw NativePresentationError("The retained Custom control is incompatible with the current codec or media settings. Choose an applicable control or None.")
        }
        guard !text.isEmpty, text.count <= 20, text.utf8.allSatisfy({ (48...57).contains($0) }),
              let value = UInt64(text), value >= option.minimum, value <= option.maximum,
              option.values.isEmpty || option.values.contains(value) else {
            throw NativePresentationError("Enter an applicable value for \(option.label) in \(option.unit).")
        }
        return value
    }
}

private enum CompressionField: String, Identifiable {
    case mode, videoControl, audioControl, videoValue, audioValue
    var id: String { rawValue }
}

@MainActor
private struct CompressionControls: View {
    @Binding var draft: CompressionDraft
    let choices: CompressionChoices?
    let video: Bool
    let hasAudio: Bool
    let error: String?
    var focus: FocusState<String?>.Binding
    @Binding var selection: CompressionField?

    private var hasVideoControls: Bool { !(choices?.video.isEmpty ?? true) }
    private var hasCompressionControls: Bool { hasVideoControls || !(choices?.audio.isEmpty ?? true) }

    var body: some View {
        Section("Compression") {
            Toggle("Reduce File Size", isOn: $draft.enabled)
                .disabled(!hasCompressionControls && !draft.enabled)
                .accessibilityIdentifier("reduce-file-size")
            if choices != nil && !hasCompressionControls {
                Text("Choose a compatible compressed codec with applicable compression controls.")
                    .font(.footnote).foregroundColor(.secondary)
            }
            if draft.enabled {
                choiceRow("Mode", value: compressionModeLabel(draft.mode), field: .mode)
                    .accessibilityIdentifier("compression-mode")
                if draft.mode == .targetSize {
                    HStack {
                        Text("Target size")
                        TextField("Positive size", text: $draft.targetText).keyboardType(.decimalPad)
                            .multilineTextAlignment(.trailing).focused(focus, equals: "target-size")
                            .accessibilityIdentifier("compression-target")
                    }
                    Picker("Unit", selection: $draft.targetUnit) {
                        Text("KB").tag(TargetSizeUnit.kilobytes)
                        Text("MB").tag(TargetSizeUnit.megabytes)
                        Text("GB").tag(TargetSizeUnit.gigabytes)
                    }.accessibilityIdentifier("compression-target-unit")
                    Text("KB = 1,000 bytes. MB = 1,000,000 bytes. GB = 1,000,000,000 bytes.")
                        .font(.footnote).foregroundColor(.secondary)
                }
                if draft.mode == .custom {
                    if video {
                        customControl("Video", options: choices?.video ?? [], kind: $draft.videoKind,
                                      value: $draft.videoValue, field: .videoControl, valueField: .videoValue)
                    }
                    if hasAudio {
                        customControl(video ? "Audio tracks" : "Audio", options: choices?.audio ?? [], kind: $draft.audioKind,
                                      value: $draft.audioValue, field: .audioControl, valueField: .audioValue)
                    }
                }
                if video {
                    Toggle("Optimize resolution", isOn: $draft.optimizeResolution)
                        .disabled(!hasVideoControls && !draft.optimizeResolution)
                        .accessibilityIdentifier("compression-optimize-resolution")
                    Toggle("Optimize frame rate", isOn: $draft.optimizeFrameRate)
                        .disabled(!hasVideoControls && !draft.optimizeFrameRate)
                        .accessibilityIdentifier("compression-optimize-frame-rate")
                    if choices != nil && !hasVideoControls {
                        Text("Video optimization requires an applicable video encoder.")
                            .font(.footnote).foregroundColor(.secondary)
                    }
                }
                if let error { Text(error).font(.footnote).foregroundColor(.red).textSelection(.enabled) }
            }
        }
    }

    private func choiceRow(_ title: String, value: String, field: CompressionField) -> some View {
        Button { selection = field } label: {
            HStack { Text(title); Spacer(); Text(value).foregroundColor(.secondary); Image(systemName: "chevron.down").font(.caption).foregroundColor(.secondary) }
        }.foregroundColor(.primary)
    }

    @ViewBuilder
    private func customControl(_ title: String, options: [CompressionControlOption], kind: Binding<CompressionControlKind?>,
                               value: Binding<String>, field: CompressionField, valueField: CompressionField) -> some View {
        choiceRow(title + " control", value: kind.wrappedValue.map { selected in
            options.first { $0.kind == selected }?.label ?? "Retained control — incompatible"
        } ?? "None", field: field)
            .accessibilityIdentifier(field == .videoControl ? "compression-video-control" : "compression-audio-control")
        if let selected = kind.wrappedValue, let option = options.first(where: { $0.kind == selected }) {
            if option.values.isEmpty {
                HStack {
                    Text(option.label)
                    TextField("Value", text: value).keyboardType(.numberPad)
                        .multilineTextAlignment(.trailing).focused(focus, equals: valueField.rawValue)
                        .accessibilityIdentifier(valueField == .videoValue ? "compression-video-value" : "compression-audio-value")
                    Text(option.unit).foregroundColor(.secondary)
                }
                Text("\(option.minimum)–\(option.maximum) \(option.unit)").font(.footnote).foregroundColor(.secondary)
            } else {
                choiceRow(option.label, value: value.wrappedValue.isEmpty ? "Choose value" : value.wrappedValue + " " + option.unit, field: valueField)
                    .accessibilityIdentifier(valueField == .videoValue ? "compression-video-value" : "compression-audio-value")
            }
        }
    }

    @ViewBuilder func sheet(_ field: CompressionField) -> some View {
        switch field {
        case .mode:
            NativeChoiceSheet(title: "Compression mode", values: [.highQuality, .balanced, .smallerFile, .targetSize, .custom],
                              selected: draft.mode, label: compressionModeLabel) { draft.mode = $0; selection = nil }
        case .videoControl, .audioControl:
            let isVideo = field == .videoControl
            let options = isVideo ? choices?.video ?? [] : choices?.audio ?? []
            let selected = isVideo ? draft.videoKind : draft.audioKind
            NativeChoiceSheet(title: isVideo ? "Video control" : "Audio control", values: [nil] + options.map { Optional($0.kind) },
                              selected: selected, label: { kind in
                kind.flatMap { selected in options.first { $0.kind == selected }?.label } ?? "None"
            }) { kind in
                if isVideo {
                    if draft.videoKind != kind {
                        draft.videoKind = kind
                        draft.videoValue = kind.flatMap { selected in options.first { $0.kind == selected }?.defaultValue }.map(String.init) ?? ""
                    }
                } else if draft.audioKind != kind {
                    draft.audioKind = kind
                    draft.audioValue = kind.flatMap { selected in options.first { $0.kind == selected }?.defaultValue }.map(String.init) ?? ""
                }
                selection = nil
            }
        case .videoValue, .audioValue:
            let isVideo = field == .videoValue
            let selected = isVideo ? draft.videoKind : draft.audioKind
            let option = (isVideo ? choices?.video ?? [] : choices?.audio ?? []).first { $0.kind == selected }
            if let option {
                NativeChoiceSheet(title: option.label, values: option.values,
                                  selected: UInt64(isVideo ? draft.videoValue : draft.audioValue) ?? UInt64.max,
                                  label: { "\($0) \(option.unit)" }) { value in
                    if isVideo { draft.videoValue = String(value) } else { draft.audioValue = String(value) }
                    selection = nil
                }
            }
        }
    }
}

private func compressionModeLabel(_ mode: CompressionMode) -> String {
    switch mode {
    case .highQuality: return "High Quality"
    case .balanced: return "Balanced Quality"
    case .smallerFile: return "Smaller File"
    case .targetSize: return "Target File Size"
    case .custom: return "Custom"
    }
}
