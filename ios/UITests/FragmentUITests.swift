import XCTest

@MainActor
final class FragmentUITests: XCTestCase {
    func testHomeStreamingStopHistoryAndWebSheet() throws {
        let app = XCUIApplication()
        app.launchArguments = ["--server", "http://localhost:8910"]
        app.launch()
        let composer = app.textFields["launcherInput"]
        XCTAssertTrue(composer.waitForExistence(timeout: 10))
        let send = app.buttons["startChat"]
        let ready = NSPredicate(format: "enabled == true")
        composer.tap()
        composer.typeText("Help me find a little more focus")
        expectation(for: ready, evaluatedWith: send)
        waitForExpectations(timeout: 10)
        send.tap()
        XCTAssertTrue(app.buttons["stop"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", "Let’s make a little room")).firstMatch.waitForExistence(timeout: 10))
        app.buttons["stop"].tap()
        XCTAssertTrue(app.buttons["send"].waitForExistence(timeout: 10))
        app.buttons["Chats"].tap()
        XCTAssertTrue(app.buttons["Reading list"].waitForExistence(timeout: 10))
        app.buttons["Reading list"].tap()
        XCTAssertTrue(app.webViews.firstMatch.waitForExistence(timeout: 10))
        XCTAssertTrue(app.webViews.staticTexts["A few good things."].waitForExistence(timeout: 10))
        app.buttons["Done"].tap()
        app.buttons["Chats"].tap()
        let search = app.searchFields.firstMatch
        search.tap()
        search.typeText("A longer conversation")
        app.buttons.matching(NSPredicate(format: "label CONTAINS %@", "A longer conversation")).firstMatch.tap()
        XCTAssertTrue(app.navigationBars["A longer conversation"].waitForExistence(timeout: 10))
        for _ in 0..<12 where !app.buttons["Earlier messages"].isHittable {
            app.scrollViews["timeline"].swipeDown(velocity: .fast)
        }
        XCTAssertTrue(app.buttons["Earlier messages"].waitForExistence(timeout: 10))
        app.buttons["Earlier messages"].tap()
        for _ in 0..<12 where !app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", "Earlier thought 1.")).firstMatch.exists {
            app.scrollViews["timeline"].swipeDown(velocity: .fast)
        }
        XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", "Earlier thought 1.")).firstMatch.waitForExistence(timeout: 10))
    }

    func testLauncherSearchesMessagesAndAppsAndReturnStartsChat() throws {
        let app = XCUIApplication()
        app.launchArguments = ["--server", "http://localhost:8910"]
        app.launch()
        let input = app.textFields["launcherInput"]
        XCTAssertTrue(input.waitForExistence(timeout: 10))
        input.tap()
        input.typeText("quiet minutes")
        let hit = app.buttons["messageResult-morning:chat:2"]
        XCTAssertTrue(hit.waitForExistence(timeout: 10))
        input.typeText(" ")
        XCTAssertTrue(hit.exists)
        XCTAssertTrue(app.buttons["newChatResult"].exists)
        let screenshot = XCTAttachment(screenshot: app.screenshot())
        screenshot.name = "Launcher search"
        screenshot.lifetime = .keepAlways
        add(screenshot)
        hit.tap()
        XCTAssertTrue(app.navigationBars["A calmer morning"].waitForExistence(timeout: 10))
        app.buttons["BackButton"].tap()
        app.buttons["Clear search"].tap()
        input.tap()
        input.typeText("Reading")
        let reading = app.buttons["fragmentResult-reading"]
        XCTAssertTrue(reading.waitForExistence(timeout: 10))
        reading.tap()
        XCTAssertTrue(app.webViews.staticTexts["A few good things."].waitForExistence(timeout: 10))
        app.buttons["Done"].tap()
        app.buttons["Clear search"].tap()
        input.tap()
        input.typeText("A fresh request from the launcher\n")
        XCTAssertTrue(app.buttons["stop"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["A fresh request from the launcher"].exists)
    }
}
