    
#[macro_use]
extern crate neon;
use std::ffi::OsString;
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;

use neon::prelude::*;

use open;

use fragment_search::{search, ListItem};

//builds were failing on linux so we found this workaround
//https://users.rust-lang.org/t/neon-electron-undefined-symbol-cxa-pure-virtual/21223/2
#[no_mangle]
pub extern "C" fn __cxa_pure_virtual() {
  loop {}
}

struct BackgroundSearch {
  argument: String,
  path: String,
}

impl Task for BackgroundSearch {
  type Output = Vec<ListItem>;
  type Error = String;
  type JsEvent = JsArray;

  fn perform(&self) -> Result<Vec<ListItem>, String> {
    let result = "pass";

    if result != "pass" {
      return Err("Did not pass lol".to_string());
    }
    
    let query = &self.argument;
    let notes_path = &self.path;
    let vec = search(&query, notes_path).expect("search didn't work");

    Ok(vec)
  }

  fn complete<'a>(
    self,
    mut cx: TaskContext<'a>,
    result: Result<Vec<ListItem>, String>
  ) -> JsResult<JsArray> {

    let list = result.unwrap();

      // Create the JS array
  let js_array = JsArray::new(&mut cx, list.len() as u32);

  // Iterate over the rust Vec and map each value in the Vec to the JS array
  for (i, obj) in list.iter().enumerate() {
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
    
}

fn query_async(mut cx: FunctionContext) -> JsResult<JsUndefined> {
  let query = cx.argument::<JsString>(0)?.value();
  let notes_path = cx.argument::<JsString>(1)?.value();
  let callback = cx.argument::<JsFunction>(2)?;

  println!("{}", query);

  let task = BackgroundSearch { argument: String::from(query), path: String::from(notes_path) };
  task.schedule(callback);

  Ok(cx.undefined())

}

fn query(mut cx: FunctionContext) -> JsResult<JsArray> {
  let query = cx.argument::<JsString>(0)?.value();
  let notes_path = cx.argument::<JsString>(1)?.value();

  // let notes_path = "../notes_grep_test";
  let list = search(&query, &notes_path).expect("search didn't work");

    // Create the JS array
  let js_array = JsArray::new(&mut cx, list.len() as u32);

  // Iterate over the rust Vec and map each value in the Vec to the JS array
  for (i, obj) in list.iter().enumerate() {
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

fn open_note_in_editor(mut cx: FunctionContext) -> JsResult<JsBoolean> {
  let path = cx.argument::<JsString>(0)?.value();
  match open::that(path) {
    Ok(exit_status) => {
      if exit_status.success() {
        println!("Check out your default editor!");
        Ok(cx.boolean(true))
      } else {
        if let Some(code) = exit_status.code() {
          println!("Command returned non-zero exit status {}!", code);
          Ok(cx.boolean(true))
        } else {
          println!("Command returned with unknown exit status!");
          Ok(cx.boolean(true))
        }
      }
    }
    Err(why) => {
      println!("Failure to execute command: {}", why);
      Ok(cx.boolean(false))
    }
  }
}

fn create_file(mut cx: FunctionContext) -> JsResult<JsBoolean> {
  let filename = cx.argument::<JsString>(0)?.value();
  let mut full_path = PathBuf::from("../notes_grep_test");

  full_path.push(filename);
  full_path.set_extension("md");

  match File::create(full_path) {
    Ok(_) => {
      println!("Made a file! Good luck finding it.");
      Ok(cx.boolean(true))
    }
    Err(why) => {
      println!("Didn't make a file: {}", why);
      Ok(cx.boolean(false))
    }
  }
}

register_module!(mut cx, {
  cx.export_function("get_note", get_note)?;
  cx.export_function("query", query)?;
  cx.export_function("open_note", open_note_in_editor)?;
  cx.export_function("create_file", create_file)?;
  cx.export_function("query_async", query_async)?;
  Ok(())
});
