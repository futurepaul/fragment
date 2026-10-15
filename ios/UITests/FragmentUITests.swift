import XCTest

@MainActor
final class FragmentUITests: XCTestCase {
    func testHomeStreamingStopHistoryAndWebSheet() throws {
        let app = XCUIApplication()
        app.launchArguments = ["--server", "http://localhost:8910"]
        app.launch()
        XCTAssertTrue(app.staticTexts["A little space\nto think big."].waitForExistence(timeout: 10))
        let composer = app.textFields["composer"]
        XCTAssertTrue(composer.waitForExistence(timeout: 10))
        let send = app.buttons["send"]
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
            app.scrollViews.firstMatch.swipeDown(velocity: .fast)
        }
        XCTAssertTrue(app.buttons["Earlier messages"].waitForExistence(timeout: 10))
        app.buttons["Earlier messages"].tap()
        for _ in 0..<12 where !app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", "Earlier thought 1.")).firstMatch.exists {
            app.scrollViews.firstMatch.swipeDown(velocity: .fast)
        }
        XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", "Earlier thought 1.")).firstMatch.waitForExistence(timeout: 10))
    }
}
