
#[macro_use]
extern crate neon;

use std::fs::File;
use std::io::prelude::*;

use neon::prelude::*;

//builds were failing on linux so we found this workaround
//https://users.rust-lang.org/t/neon-electron-undefined-symbol-cxa-pure-virtual/21223/2
#[no_mangle]
pub extern fn __cxa_pure_virtual() {
    loop{};
}

fn run_query(query: &String) -> Vec<String> {

    let query = query.clone();

     let vec: Vec<String> = vec![query; 10];
     vec
}

fn query(mut cx: FunctionContext) -> JsResult<JsArray> {
    let query = cx.argument::<JsString>(0)?.value();
    let vec = run_query(&query);

    // Create the JS array
    let js_array = JsArray::new(&mut cx, vec.len() as u32);

    // Iterate over the rust Vec and map each value in the Vec to the JS array
    for (i, obj) in vec.iter().enumerate() {
        let js_string = cx.string(obj);
        js_array.set(&mut cx, i as u32, js_string).unwrap();
    }

    Ok(js_array) 
}

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
    cx.export_function("query", query)?;
    Ok(())
});
