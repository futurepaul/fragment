"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
// This file is required by the index.html file and will
// be executed in the renderer process for that window.
// All of the Node.js APIs are available in this process.
var fragment = require("frag_native");
var apprun_1 = require("apprun");
try {
    document.write("<pre>" + fragment.hello() + "</pre>");
}
catch (e) {
    document.write("<pre>" + e.stack + "</pre>");
}
document.write("<h1>test</h1>");
var state = {
    list: ["result1", "result2", "result3"]
};
var view = function (state) { return (apprun_1.default.createElement("div", null,
    apprun_1.default.createElement("h1", null, "type something"),
    apprun_1.default.createElement("input", { type: "text", oninput: function (_e) { return apprun_1.default.run("update-query", _e); }, onkeypress: function (e) { return apprun_1.default.run("keypress", e); } }),
    state.list.map(function (item, key) { return (apprun_1.default.createElement("div", { key: key }, item)); }))); };
var update = {
    keypress: function (_, e) {
        e.keyCode === 13 && apprun_1.default.run("update-query", e);
    },
    "update-query": function (state, e) {
        var input = e.target.value;
        var response = [];
        try {
            response = fragment.query(input) || [];
            console.log(response);
        }
        catch (e) {
            console.log(e);
        }
        console.log(response);
        return { list: response };
    }
};
apprun_1.default.start("my-app", state, view, update);
//# sourceMappingURL=renderer.js.map