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

fn greeting(mut cx: FunctionContext) -> JsResult<JsString> {
  let name = cx.argument::<JsString>(0)?.value();
  Ok(cx.string(format!("hello, {}", name)))

}

register_module!(mut cx, {
    cx.export_function("hello", hello)?;
    cx.export_function("greeting", greeting)?;
    Ok(())
});
