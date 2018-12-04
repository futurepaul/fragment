#[macro_use]
extern crate neon;

use std::fs::File;
use std::io::prelude::*;

use neon::prelude::*;

fn hello(mut cx: FunctionContext) -> JsResult<JsString> {
    let mut file = File::open("package.json").unwrap();
    let mut contents = String::new();
    file.read_to_string(&mut contents).unwrap();
    Ok(cx.string(contents))
}

register_module!(mut cx, {
    cx.export_function("hello", hello)
});
