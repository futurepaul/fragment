extern crate grep;
#[macro_use]
extern crate neon;
extern crate walkdir;

use grep::regex::RegexMatcher;
use grep::searcher::sinks::UTF8;
use grep::searcher::{BinaryDetection, SearcherBuilder};

use std::fs::File;
use std::io::prelude::*;
use std::time::{Duration, Instant};

use std::ffi::OsString;

use walkdir::{DirEntry, WalkDir};

use neon::prelude::*;

use std::error::Error;

fn is_hidden(entry: &DirEntry) -> bool {
  entry
    .file_name()
    .to_str()
    .map(|s| s.starts_with("."))
    .unwrap_or(false)
}

//builds were failing on linux so we found this workaround
//https://users.rust-lang.org/t/neon-electron-undefined-symbol-cxa-pure-virtual/21223/2
#[no_mangle]
pub extern "C" fn __cxa_pure_virtual() {
  loop {}
}

struct ListItem {
  pub path: String,
  pub line: String,
  pub line_num: u64,
}

fn grep_life(pattern: &String) -> Result<Vec<ListItem>, Box<Error>> {
  let mut matches: Vec<ListItem> = vec![];
  let matcher = RegexMatcher::new(&pattern)?;
  let dir = OsString::from("./node_modules/");
  // let dir = OsString::from("../notes_grep_test/");
  let mut searcher = SearcherBuilder::new()
    .binary_detection(BinaryDetection::quit(b'\x00'))
    .build();

  //hack to keep search times low. we give up after 10ms
  let start_time = Instant::now();
  let timeout_duration = Duration::from_millis(10);
  let end_time = start_time + timeout_duration;

  for result in WalkDir::new(dir)
    .into_iter()
    .filter_entry(|e| !is_hidden(e))
    .take_while(|_| Instant::now() < end_time)
  {
    let dent = match result {
      Ok(dent) => dent,
      Err(err) => {
        eprintln!("{}", err);
        continue;
      }
    };
    if !dent.file_type().is_file() {
      continue;
    }
    let result = searcher.search_path(
      &matcher,
      dent.path(),
      UTF8(|lnum, line| {
        //TODO: this is a hacky way to convert the path to the string
        // and get rid of the extra quotation marks that :? adds
        let path = format!("{:?}", dent.path());
        let trimmed_path = path.trim_start_matches('"').trim_end_matches('"');
        matches.push(ListItem {
          path: trimmed_path.to_string(),
          line: line.to_string(),
          line_num: lnum,
        });
        Ok(true)
      }),
    );
    if let Err(err) = result {
      eprintln!("{}: {}", dent.path().display(), err);
    }
    //TODO: convert this to a "take" and somehow add more as
    //the user scrolls
    if matches.len() >= 10 {
      break;
    }
  }

  Ok(matches)
}

fn query(mut cx: FunctionContext) -> JsResult<JsArray> {
  let query = cx.argument::<JsString>(0)?.value();

  //TODO: fix this unwrap
  let vec = grep_life(&query).unwrap();

  // Create the JS array
  let js_array = JsArray::new(&mut cx, vec.len() as u32);

  // Iterate over the rust Vec and map each value in the Vec to the JS array
  for (i, obj) in vec.iter().enumerate() {
    let list_item_object = JsObject::new(&mut cx);
    let js_path = cx.string(&obj.path);
    let js_line = cx.string(&obj.line);
    let js_line_num = cx.number(obj.line_num as f64);
    list_item_object.set(&mut cx, "path", js_path).unwrap();
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
  cx.export_function("get_note", get_note)?;
  Ok(())
});
