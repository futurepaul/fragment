use std::ffi::OsString;
use std::fs::File;
use std::io::Read;

use neon::context::{Context, FunctionContext};
use neon::object::Object;
use neon::register_module;
use neon::result::JsResult;
use neon::types::{JsArray, JsObject, JsString};

use fragment_search::{index, search};

//builds were failing on linux so we found this workaround
//https://users.rust-lang.org/t/neon-electron-undefined-symbol-cxa-pure-virtual/21223/2
#[no_mangle]
pub extern "C" fn __cxa_pure_virtual() {
  loop {}
}

fn query(mut cx: FunctionContext) -> JsResult<JsArray> {
  let query = cx.argument::<JsString>(0)?.value();

  //TODO: fix this unwrap
  let index_storage_path = "../notes_grep_test/.index_storage";
  let notes_path = "../notes_grep_test";
  let (index, how_many_indexed) =
    index(notes_path, index_storage_path, true).expect("index failed");
  // println!("indexed {} documents!", how_many_indexed);
  let vec = search(index, &query).expect("search didn't work");

  // Create the JS array
  let js_array = JsArray::new(&mut cx, vec.len() as u32);

  // Iterate over the rust Vec and map each value in the Vec to the JS array
  for (i, obj) in vec.iter().enumerate() {
    let list_item_object = JsObject::new(&mut cx);
    let js_path = cx.string(&obj.path);
    let js_file_name = cx.string(&obj.file_name);
    let js_first_line = cx.string(&obj.first_line);

    list_item_object.set(&mut cx, "path", js_path).unwrap();
    list_item_object
      .set(&mut cx, "file_name", js_file_name)
      .unwrap();
    list_item_object
      .set(&mut cx, "first_line", js_first_line)
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
  cx.export_function("get_note", get_note)?;
  cx.export_function("query", query)?;
  Ok(())
});
