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

pub type FileList = Vec<search::ListItem>;

declare_types! {
  pub class JsFileList for FileList {
    init(mut cx) {
      let root: String = cx.argument::<JsString>(0)?.value();
      let files = search::list_of_all_files(&root, search::SortMethod::DateNewest);
      Ok(files)
    }

    method search(mut cx) {

      let query: String = cx.argument::<JsString>(0)?.value();

      let this = cx.this();
      let vec = {
        let guard = cx.lock();
        let vec = search::grep_life(&query, &this.borrow(&guard));
        vec.unwrap()
      };

      let js_array = JsArray::new(&mut cx, vec.len() as u32);
      // Iterate over the rust Vec and map each value in the Vec to the JS array
      // TODO: can't take 100 forever, gotta base it on the scroll position
      for (i, obj) in vec.iter().take(100).enumerate() {
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

      Ok(js_array.upcast())
    }

    method first(mut cx) {
      let this = cx.this();
      let first = {
        let guard = cx.lock();
        let borrowed = this.borrow(&guard);
        borrowed[0].path.clone()
      };
      println!("{:?}", first);
      Ok(cx.undefined().upcast())
    }
  }
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
  cx.export_class::<JsFileList>("FileList")?;
  Ok(())
});
