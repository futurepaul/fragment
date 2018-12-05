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
var state = "search query";
var view = function (state) { return (apprun_1.default.createElement("div", null,
    apprun_1.default.createElement("h1", null, state),
    apprun_1.default.createElement("input", { type: "text", onkeypress: function (e) { return apprun_1.default.run("keypress", e); } }))); };
var update = {
    keypress: function (_, e) {
        e.keyCode === 13 && apprun_1.default.run("update-query");
    },
    "update-query": function (state) {
        var input = document.querySelector("input");
        var response = "";
        try {
            response = fragment.query(input.value) || "";
        }
        catch (e) {
            console.log(e);
        }
        console.log(response);
        return "";
    }
};
apprun_1.default.start("my-app", state, view, update);
//# sourceMappingURL=renderer.js.map