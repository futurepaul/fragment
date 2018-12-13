#![feature(duration_as_u128)]

#[macro_use]
extern crate neon;

use std::fs::File;
use std::io::prelude::*;

use std::ffi::OsString;

use neon::prelude::*;

mod search;

//builds were failing on linux so we found this workaround
//https://users.rust-lang.org/t/neon-electron-undefined-symbol-cxa-pure-virtual/21223/2
#[no_mangle]
pub extern "C" fn __cxa_pure_virtual() {
  loop {}
}

struct SearchTask {
  query: String,
}

impl Task for SearchTask {
  type Output = Vec<search::ListItem>;
  type Error = ();
  type JsEvent = JsArray;

  fn perform(&self) -> Result<Vec<search::ListItem>, ()> {
    //probably bad to unwrap here
    Ok(search::search_king(&self.query).unwrap())
  }

  fn complete<'a>(
    self,
    mut cx: TaskContext<'a>,
    result: Result<Vec<search::ListItem>, ()>,
  ) -> JsResult<JsArray> {
    //more unwrapping!
    let vec = result.unwrap();

    // Create the JS array
    let js_array = JsArray::new(&mut cx, vec.len() as u32);

    // Iterate over the rust Vec and map each value in the Vec to the JS array
    for (i, obj) in vec.iter().enumerate() {
      let list_item_object = JsObject::new(&mut cx);
      let js_path = cx.string(&obj.path);
      let js_file_name = cx.string(&obj.file_name);
      let js_line = cx.string(match &obj.line {
        Some(line) => line,
        None => "",
      });
      let js_line_num = cx.number(match obj.line_num {
        Some(line_num) => line_num as f64,
        None => 0 as f64,
      });
      list_item_object.set(&mut cx, "path", js_path).unwrap();
      list_item_object
        .set(&mut cx, "file_name", js_file_name)
        .unwrap();
      list_item_object.set(&mut cx, "line", js_line).unwrap();
      list_item_object
        .set(&mut cx, "line_num", js_line_num)
        .unwrap();
      js_array.set(&mut cx, i as u32, list_item_object).unwrap();
    }

    Ok(js_array)
  }
}

// fn vec_item_to_js(mut cx: FunctionContext, vec: Vec<ListItem>) - JsResult<JsArray

fn query_async(mut cx: FunctionContext) -> JsResult<JsUndefined> {
  let query = cx.argument::<JsString>(0)?.value();
  //what's cb stand for?
  let cb = cx.argument::<JsFunction>(1)?;

  let task = SearchTask { query: query };
  task.schedule(cb);

  Ok(cx.undefined())
}

fn query(mut cx: FunctionContext) -> JsResult<JsArray> {
  let query = cx.argument::<JsString>(0)?.value();

  //TODO: fix this unwrap
  let vec = search::search_king(&query).unwrap();

  // Create the JS array
  let js_array = JsArray::new(&mut cx, vec.len() as u32);

  // Iterate over the rust Vec and map each value in the Vec to the JS array
  for (i, obj) in vec.iter().enumerate() {
    let list_item_object = JsObject::new(&mut cx);
    let js_path = cx.string(&obj.path);
    let js_file_name = cx.string(&obj.file_name);
    let js_line = cx.string(match &obj.line {
      Some(line) => line,
      None => "",
    });
    let js_line_num = cx.number(match obj.line_num {
      Some(line_num) => line_num as f64,
      None => 0 as f64,
    });
    list_item_object.set(&mut cx, "path", js_path).unwrap();
    list_item_object
      .set(&mut cx, "file_name", js_file_name)
      .unwrap();
    list_item_object.set(&mut cx, "line", js_line).unwrap();
    list_item_object
      .set(&mut cx, "line_num", js_line_num)
      .unwrap();
    js_array.set(&mut cx, i as u32, list_item_object).unwrap();
  }

  Ok(js_array)
}

//TODO: for really long notes consider only providing some
// of the note at a time
fn get_note(mut cx: FunctionContext) -> JsResult<JsString> {
  let path = cx.argument::<JsString>(0)?.value();
  //TODO: handle this unwrap better
  let mut file = File::open(OsString::from(path)).unwrap();
  let mut contents = String::new();
  file.read_to_string(&mut contents).unwrap();
  Ok(cx.string(contents))
}

register_module!(mut cx, {
  cx.export_function("query", query)?;
  cx.export_function("query_async", query_async)?;
  cx.export_function("get_note", get_note)?;
  Ok(())
});
