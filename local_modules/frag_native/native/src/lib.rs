#![feature(duration_as_u128)]

#[macro_use]
extern crate neon;

use std::fs::File;
use std::io::prelude::*;
use std::path::PathBuf;

use std::ffi::OsString;

use neon::prelude::*;

mod tantivy_search;

//builds were failing on linux so we found this workaround
//https://users.rust-lang.org/t/neon-electron-undefined-symbol-cxa-pure-virtual/21223/2
#[no_mangle]
pub extern "C" fn __cxa_pure_virtual() {
  loop {}
}

fn query(mut cx: FunctionContext) -> JsResult<JsArray> {
  let query = cx.argument::<JsString>(0)?.value();

  //TODO: fix this unwrap
  let index_storage_path = PathBuf::from("../notes_grep_test/.index_storage");
  let (index, how_many_indexed) =
    tantivy_search::build_index("../notes_grep_test", index_storage_path).unwrap();
  println!("indexed {} documents!", how_many_indexed);
  let vec = tantivy_search::search(index, &query).expect("search didn't work");

  // Create the JS array
  let js_array = JsArray::new(&mut cx, vec.len() as u32);

  // Iterate over the rust Vec and map each value in the Vec to the JS array
  for (i, obj) in vec.iter().enumerate() {
    let list_item_object = JsObject::new(&mut cx);
    let js_path = cx.string(&obj.path);
    let js_file_name = cx.string(&obj.file_name);

    list_item_object.set(&mut cx, "path", js_path).unwrap();
    list_item_object
      .set(&mut cx, "file_name", js_file_name)
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
