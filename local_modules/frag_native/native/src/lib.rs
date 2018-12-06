extern crate grep;
#[macro_use]
extern crate neon;
extern crate walkdir;

use grep::matcher::Matcher;
use grep::regex::RegexMatcher;
use grep::searcher::sinks::UTF8;
use grep::searcher::Searcher;
use grep::searcher::{BinaryDetection, SearcherBuilder};

use std::fs::File;
use std::io::prelude::*;
use std::path::Path;
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

fn grep_life(pattern: &String) -> Result<Vec<String>, Box<Error>> {
  let mut matches: Vec<String> = vec![];
  let matcher = RegexMatcher::new(&pattern)?;
  // let path = Path::new("./node_modules/");
  let dir = OsString::from("./node_modules/");
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
        // let mymatch = matcher.find(line.as_bytes())?.unwrap();
        matches.push(format!(
          "file: {:?} line#: {} - {}",
          dent.path(),
          lnum.to_string(),
          line.to_string()
        ));
        Ok(true)
      }),
    );
    if let Err(err) = result {
      eprintln!("{}: {}", dent.path().display(), err);
    }
    if matches.len() >= 10 {
      break;
    }
  }

  Ok(matches)
}

fn run_query(query: &String) -> Vec<String> {
  let query = query.clone();

  // let vec: Vec<String> = vec![query; 10];
  let vec = grep_life(&query).unwrap();
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
