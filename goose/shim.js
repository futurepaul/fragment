import * as imports from "./index_bg.js";
export * from "./index_bg.js";
import wasmModule from "./index.wasm";
$SNIPPET_JS_IMPORTS
const instance = new WebAssembly.Instance(wasmModule, {
  "./index_bg.js": imports,
  $SNIPPET_WASM_IMPORTS
});
imports.__wbg_set_wasm(instance.exports);
instance.exports.__wbindgen_start?.();
