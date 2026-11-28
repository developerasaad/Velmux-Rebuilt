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
        try waitFor("The native Home screen did not become ready", timeout: 45) {
            self.app.buttons["tool-video-converter"].isHittable
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
        try choose(id: "output-format", option: "MP4")
        try choose(id: "video-codec", option: "H.264")
        let stem = "picker-video-" + UUID().uuidString.lowercased()
        try setOutputName(stem)
        capture("Video workspace — actual selected source and H.264")
        try tap(label: "Convert", scroll: true)
        try waitForCompletedOutput(stem + ".mp4")
        capture("Jobs — actual picker video conversion completed")
        try verifySettingsPersistenceAndRestore()
    }

    func testActualFilesPickerAudioConversion() throws {
        try tap(label: "Audio Converter", scroll: true)
        try selectSeededDocument("stereo.wav")
        try waitForWorkspace(source: "stereo.wav")
        try choose(id: "output-format", option: "M4A")
        try choose(id: "audio-codec", option: "AAC-LC")
        let stem = "picker-audio-" + UUID().uuidString.lowercased()
        try setOutputName(stem)
        capture("Audio workspace — actual selected source and AAC")
        try tap(label: "Convert", scroll: true)
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
            self.app.descendants(matching: .any).matching(identifier: "source-name").firstMatch.exists
                && self.element(label: source).exists
        }
        XCTAssertFalse(app.buttons["Cancel"].exists, "The system picker must dismiss after selection")
        capture("Workspace — inspected \(source)")
    }

    private func choose(id: String, option: String) throws {
        let row = app.buttons[id]
        for _ in 0..<6 { if row.isHittable { break }; app.swipeUp() }
        try waitFor("The native choice row \(id) was not reachable") { row.isHittable }
        row.tap()
        try tap(label: option)
        try waitFor("The native choice sheet did not apply \(option)") { row.label.contains(option) }
    }

    private func setOutputName(_ stem: String) throws {
        let field = app.textFields["output-filename"]
        for _ in 0..<6 { if field.isHittable { break }; app.swipeUp() }
        try waitFor("The native filename field was not reachable") { field.isHittable }
        field.tap()
        let current = (field.value as? String) ?? ""
        guard !current.isEmpty, current.count <= 128 else { throw failure("The filename default was not exposed") }
        field.typeText(String(repeating: XCUIKeyboardKey.delete.rawValue, count: current.count))
        field.typeText(stem)
        try waitFor("The native filename did not update") { (field.value as? String) == stem }
        let keyboard = app.keyboards
        for title in ["Done", "Return"] {
            let button = keyboard.buttons[title].firstMatch
            if button.exists && button.isHittable { button.tap(); break }
        }
    }

    private func waitForCompletedOutput(_ name: String) throws {
        let section = app.descendants(matching: .any).matching(identifier: "job-" + name).firstMatch
        try waitFor("The accepted native job did not publish \(name)", timeout: 90) {
            section.exists && section.descendants(matching: .staticText).matching(NSPredicate(format: "label == %@", "Completed")).firstMatch.exists
        }
        XCTAssertTrue(section.descendants(matching: .button).matching(NSPredicate(format: "label == %@", "Open")).firstMatch.exists)
        XCTAssertTrue(section.descendants(matching: .button).matching(NSPredicate(format: "label == %@", "Share")).firstMatch.exists)
        XCTAssertFalse(section.descendants(matching: .staticText).matching(NSPredicate(format: "label == %@", "Failed")).firstMatch.exists)
    }

    private func verifySettingsPersistenceAndRestore() throws {
        for _ in 0..<3 {
            if app.buttons["tool-video-converter"].isHittable { break }
            let back = app.navigationBars.buttons.element(boundBy: 0)
            try waitFor("Native navigation did not expose Back") { back.isHittable }
            back.tap()
        }
        try waitFor("Native navigation did not return Home") { self.app.buttons["tool-video-converter"].isHittable }
        app.buttons["settings-action"].tap()
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
                    app.buttons["settings-action"].tap()
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
        app.buttons["settings-action"].tap()
        try waitFor("Appearance did not persist across application relaunch") { self.appearanceSelected(changed) }
        capture("Settings — \(changed.lowercased()) appearance persisted after relaunch")
    }

    private func appearanceSelected(_ label: String) -> Bool {
        let option = app.buttons.matching(NSPredicate(format: "label == %@", label)).firstMatch
        guard option.exists else { return false }
        return option.isSelected || (option.value as? String) == "1" || (option.value as? NSNumber)?.boolValue == true
    }

    private func waitForHome() throws {
        try waitFor("The native Home screen did not become ready after relaunch", timeout: 45) {
            self.app.buttons["tool-video-converter"].isHittable
        }
    }

    private func element(label: String) -> XCUIElement {
        // Files can append traits after the visible name. Native controls can
        // combine their current value with their title in one accessible label.
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
