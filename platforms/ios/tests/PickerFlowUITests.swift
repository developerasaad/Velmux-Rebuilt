import XCTest

/// Exercises the actual system document picker. CI seeds generated fixtures in
/// the separate fixture application's Documents/UITestSources before this target
/// runs. Selecting its Files entry crosses the Velmux application's sandbox.
/// No application launch hook or direct picker-delegate invocation supplies URLs.
final class PickerFlowUITests: XCTestCase {
    private var app: XCUIApplication!

    override func setUpWithError() throws {
        continueAfterFailure = false
        app = XCUIApplication()
        app.launchArguments += ["-AppleLanguages", "(en)", "-AppleLocale", "en_US"]
        app.launch()
        try waitFor("The shared Home screen did not become ready", timeout: 45) {
            self.element(label: "Video Converter").isHittable
                && !self.element(label: "Restoring the workspace…").exists
                && !self.element(label: "Media engine unavailable").exists
        }
    }

    override func tearDownWithError() throws {
        if testRun?.hasSucceeded == false {
            capture("Failure — visible application and system picker")
            let hierarchy = XCTAttachment(string: app.debugDescription)
            hierarchy.name = "Failure accessibility hierarchy"
            hierarchy.lifetime = .keepAlways
            add(hierarchy)
        }
        app.terminate()
    }

    func testActualFilesPickerCancellationAndVideoConversion() throws {
        capture("Home before actual document selection")
        try tap(label: "Video Converter")
        let cancel = app.buttons["Cancel"].firstMatch
        try waitFor("The system document picker did not present Cancel") { cancel.isHittable }
        capture("System video picker before cancellation")
        cancel.tap()
        try waitFor("Cancel did not return to Home") {
            self.element(label: "Video Converter").isHittable && !self.element(label: "Convert").exists
        }
        XCTAssertFalse(element(label: "Source unavailable").exists, "System picker cancellation must not become a source error")
        XCTAssertFalse(element(label: "Source preparation failed").exists)
        capture("Home after actual picker cancellation")

        try tap(label: "Video Converter")
        // CI seeds the same generated MP4 bytes under a generic-data extension
        // in the external fixture app. The actual picker must not hide it based
        // on native media UTIs; successful conversion requires Rust inspection.
        try selectSeededDocument("video.bin")
        try waitForWorkspace(source: "video.bin")
        try choose(title: "Codec", option: "H.264")
        let stem = "picker-video-" + UUID().uuidString.lowercased()
        try setOutputName(stem)
        capture("Video workspace — actual selected source and H.264")
        try tap(label: "Convert")
        try waitForCompletedOutput(stem + ".mp4")
        capture("Jobs — actual picker video conversion completed")
        try verifySettingsPersistenceAndRestore()
    }

    func testActualFilesPickerAudioConversion() throws {
        try tap(label: "Audio Converter", scroll: true)
        try selectSeededDocument("stereo.wav")
        try waitForWorkspace(source: "stereo.wav")
        try choose(title: "Audio codec", option: "AAC")
        let stem = "picker-audio-" + UUID().uuidString.lowercased()
        try setOutputName(stem)
        capture("Audio workspace — actual selected source and AAC")
        try tap(label: "Convert")
        try waitForCompletedOutput(stem + ".m4a")
        capture("Jobs — actual picker audio conversion completed")
    }

    private func selectSeededDocument(_ fileName: String) throws {
        try waitFor("The system document picker was not presented") {
            self.app.buttons["Cancel"].firstMatch.isHittable
        }
        // A picker may reopen its previous directory. Navigate through its
        // visible Browse/Locations controls, never a synthetic URL/delegate call.
        var location: XCUIElement?
        for _ in 0..<6 {
            location = ["On My iPhone", "On My iPad"].map { element(label: $0) }
                .first(where: { $0.exists && $0.isHittable })
            if location != nil { break }
            let browse = app.tabBars.buttons["Browse"].firstMatch
            let back = app.navigationBars.buttons.matching(NSPredicate(format: "label == 'Browse' OR label == 'Locations'")).firstMatch
            if back.exists && back.isHittable { back.tap() }
            else if browse.exists && browse.isHittable { browse.tap() }
            else if element(label: "Browse").isHittable { element(label: "Browse").tap() }
            else if element(label: "Locations").isHittable { element(label: "Locations").tap() }
            else { break }
        }
        guard let local = location else {
            capture("System picker — local Files location unavailable")
            throw failure("The real system picker did not expose On My iPhone/iPad; seeded fixture selection is unverified")
        }
        local.tap()
        try tap(label: "Velmux Media Fixtures")
        try tap(label: "UITestSources")
        capture("System Files picker — generated fixture folder")
        // shouldShowFileExtensions is enabled, but accept Files' extensionless
        // accessibility label when that system presentation is used.
        let base = (fileName as NSString).deletingPathExtension
        try waitFor("The seeded fixture \(fileName) was not visible in the actual Files picker") {
            self.element(label: fileName).isHittable || self.element(label: base).isHittable
        }
        let file = element(label: fileName).isHittable ? element(label: fileName) : element(label: base)
        // Files' icon-mode cell center falls on the filename/badge region for
        // a movie thumbnail. Tap the actual enabled file icon when available.
        let icon = file.images.firstMatch
        if icon.exists && icon.isHittable { icon.tap() }
        else { file.tap() }
        // iOS versions presenting an explicit Open action still use the real
        // picker action. Single-selection versions dismiss on the file tap.
        let open = app.navigationBars.buttons["Open"].firstMatch
        if open.waitForExistence(timeout: 1), open.isHittable { open.tap() }
    }

    private func waitForWorkspace(source: String) throws {
        try waitFor("The selected source did not open a converter workspace", timeout: 45) {
            self.element(label: source).exists && self.element(label: "Convert").isHittable
        }
        XCTAssertFalse(app.buttons["Cancel"].exists, "The system picker must dismiss after selection")
        capture("Workspace — inspected \(source)")
    }

    private func choose(title: String, option: String) throws {
        let tagged = app.descendants(matching: .any).matching(identifier: "choice-" + title).firstMatch
        if tagged.exists && tagged.isHittable { tagged.tap() }
        else { try tap(label: title, scroll: true) }
        try tap(label: option)
        try waitFor("The \(title) choice was not applied to the workspace") { tagged.label.contains(option) }
    }

    private func setOutputName(_ stem: String) throws {
        let tagged = app.descendants(matching: .any).matching(identifier: "output-name").firstMatch
        for _ in 0..<6 {
            if tagged.exists && tagged.isHittable { break }
            app.swipeUp()
        }
        let field = tagged.exists ? tagged : app.textFields.matching(NSPredicate(format: "label CONTAINS 'Output name'")).firstMatch
        try waitFor("The output-name field was not accessible") { field.exists && field.isHittable }
        field.tap()
        let keyboardContinue = app.buttons["Continue"].firstMatch
        if keyboardContinue.waitForExistence(timeout: 2), keyboardContinue.isHittable { keyboardContinue.tap() }
        // Compose exports this editable TextView's content as its accessibility
        // label on iOS; UIKit text fields normally expose the same text as value.
        let current = (field.value as? String).flatMap { $0.isEmpty ? nil : $0 } ?? field.label
        guard !current.isEmpty, current != "Output name" else {
            throw failure("The output-name field did not expose its editable value")
        }
        // Clear and verify before inserting. Combining deletion and insertion
        // after a selection gesture can leave an old prefix in the real editor.
        field.typeText(String(repeating: XCUIKeyboardKey.delete.rawValue, count: current.count + 8))
        try waitFor("The previous output name was not cleared", timeout: 10) {
            let text = (field.value as? String).flatMap { $0.isEmpty ? nil : $0 } ?? field.label
            return text.isEmpty || text == "Output name"
        }
        field.typeText(stem)
        try waitFor("The edited output name did not identify this request uniquely") {
            (field.value as? String) == stem || field.label == stem
        }
        for title in ["Done", "Return"] {
            let key = app.keyboards.buttons[title].firstMatch
            if key.exists && key.isHittable { key.tap(); break }
        }
    }

    private func waitForCompletedOutput(_ fileName: String) throws {
        let card = app.descendants(matching: .any).matching(identifier: "job-" + fileName).firstMatch
        try waitFor("The accepted request did not appear in Jobs", timeout: 45) { card.exists }
        try waitFor("The actual picker conversion did not complete: \(fileName)", timeout: 120) {
            self.elementWithin(card: card, label: "Completed", type: .staticText) != nil
        }
        XCTAssertNotNil(elementWithin(card: card, label: fileName, type: .any), "The card must identify this exact request")
        XCTAssertNotNil(elementWithin(card: card, label: "Open", type: .button), "This completed result must have its own reachable Open action")
        XCTAssertNotNil(elementWithin(card: card, label: "Share", type: .button), "This completed result must have its own reachable Share action")
        XCTAssertNil(elementWithin(card: card, label: "Failed", type: .staticText))
    }

    private func elementWithin(card: XCUIElement, label: String, type: XCUIElement.ElementType) -> XCUIElement? {
        // Compose's iOS LazyColumn flattens the tagged card, status and actions
        // into accessibility siblings. Its unique tag still exposes the actual
        // card bounds; require the exact label and positive, reachable geometry
        // inside those bounds rather than accepting another job's completion.
        guard card.exists else { return nil }
        let bounds = card.frame
        return app.descendants(matching: type).matching(NSPredicate(format: "label == %@", label))
            .allElementsBoundByIndex.first { candidate in
                let frame = candidate.frame
                return frame.width > 0 && frame.height > 0 && bounds.contains(frame)
                    && candidate.isHittable && candidate.isEnabled
            }
    }

    private func verifySettingsPersistenceAndRestore() throws {
        try tap(label: "Back")
        try tap(label: "Settings")
        try waitFor("Settings did not expose appearance controls") { self.element(label: "Appearance").exists }
        XCTAssertTrue(element(label: "Files app → Velmux → Velmux").exists, "Settings must show the user-accessible Files location")
        let choices = ["System", "Light", "Dark"]
        guard let original = choices.first(where: { appearanceSelected($0) }) else {
            throw failure("Settings did not expose its selected appearance")
        }
        capture("Settings — original appearance and output location")
        let changed = original == "Dark" ? "Light" : "Dark"
        defer {
            // Restore the actual prior preference even when a persistence
            // assertion fails. A cleanup failure remains a test failure.
            do {
                if !element(label: "Appearance").exists {
                    app.terminate()
                    app.launch()
                    try waitForHome()
                    try tap(label: "Settings")
                }
                try tap(label: original)
                try waitFor("The original appearance preference was not restored") { self.appearanceSelected(original) }
            } catch {
                XCTFail("Unable to restore the original appearance: \(error)")
            }
        }
        try tap(label: changed)
        try waitFor("The selected appearance was not applied") { self.appearanceSelected(changed) }
        capture("Settings — \(changed.lowercased()) appearance applied")
        app.terminate()
        app.launch()
        try waitForHome()
        try tap(label: "Settings")
        try waitFor("Appearance did not persist across application relaunch") { self.appearanceSelected(changed) }
        capture("Settings — \(changed.lowercased()) appearance persisted after relaunch")
    }

    private func appearanceSelected(_ label: String) -> Bool {
        let option = app.buttons.matching(NSPredicate(format: "label == %@", label)).firstMatch
        guard option.exists else { return false }
        return option.isSelected || (option.value as? String) == "1" || (option.value as? NSNumber)?.boolValue == true
    }

    private func waitForHome() throws {
        try waitFor("The shared Home screen did not become ready after relaunch", timeout: 45) {
            self.element(label: "Video Converter").isHittable
                && !self.element(label: "Restoring the workspace…").exists
                && !self.element(label: "Media engine unavailable").exists
        }
    }

    private func element(label: String) -> XCUIElement {
        // Compose merges text into button/clickable-row labels. Files cells add
        // traits after a comma. Match the visible leading label without making
        // assumptions about UIKit versus Compose element types.
        let query = app.descendants(matching: .any).matching(NSPredicate(
            format: "label == %@ OR label BEGINSWITH %@ OR label BEGINSWITH %@",
            label, label + "\n", label + ", "
        ))
        return query.allElementsBoundByIndex.first(where: { $0.isHittable }) ?? query.firstMatch
    }

    private func tap(label: String, scroll: Bool = false) throws {
        if scroll {
            for _ in 0..<6 {
                if element(label: label).isHittable { break }
                app.swipeUp()
            }
        }
        try waitFor("The visible control '\(label)' was not reachable") { self.element(label: label).isHittable }
        element(label: label).tap()
    }

    private func waitFor(_ description: String, timeout: TimeInterval = 20, condition: @escaping () -> Bool) throws {
        let expectation = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in condition() }, object: nil)
        guard XCTWaiter.wait(for: [expectation], timeout: timeout) == .completed else {
            capture(description)
            throw failure(description)
        }
    }

    private func capture(_ name: String) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    private func failure(_ description: String) -> NSError {
        NSError(domain: "Velmux.PickerUITests", code: 1, userInfo: [NSLocalizedDescriptionKey: description])
    }
}
