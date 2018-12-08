use grep::regex::RegexMatcher;
use grep::searcher::sinks::UTF8;
use grep::searcher::{BinaryDetection, SearcherBuilder};
use walkdir::{DirEntry, WalkDir};

use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::time::{Duration, Instant, SystemTime};

pub struct ListItem {
  pub path: String,
  pub file_name: String,
  pub modified: SystemTime,
  pub line: Option<String>,
  pub line_num: Option<u64>,
}

// impl Iterator for ListItem {
//   type Item =
// }

fn is_hidden(entry: &DirEntry) -> bool {
  entry
    .file_name()
    .to_str()
    .map(|s| s.starts_with('.'))
    .unwrap_or(false)
}

enum SortMethod {
  DateNewest,
  DateOldest,
  TitleAZ,
  TitleZA,
  NoSort,
}

pub fn search_king(pattern: &str) -> Result<Vec<ListItem>, Box<Error>> {
  // let root = "./node_modules";
  let root = "../notes_grep_test";
  let files = list_of_all_files(root, SortMethod::DateNewest);

  grep_life(pattern, files)
}

fn list_of_all_files(root: &str, sort_by: SortMethod) -> Vec<ListItem> {
  let list_start = Instant::now();
  let dir = OsString::from(root);

  let mut list = WalkDir::new(dir)
    .into_iter()
    .filter_entry(|e| !is_hidden(e))
    .inspect(|result| {
      if let Err(ref e) = *result {
        eprintln!("{}", e);
      }
    })
    .filter_map(Result::ok)
    .filter(|dent| dent.file_type().is_file())
    .map(|dent| ListItem {
      path: dent.path().display().to_string(),
      file_name: dent.file_name().to_os_string().into_string().unwrap(),
      modified: get_modified_time(&dent),
      //can we read the first line of each file here without crying?
      line: None,
      line_num: None,
    })
    .collect::<Vec<ListItem>>();

  match sort_by {
    SortMethod::DateNewest => list.sort_unstable_by(|a, b| b.modified.cmp(&a.modified)),
    SortMethod::DateOldest => list.sort_unstable_by(|a, b| a.modified.cmp(&b.modified)),
    SortMethod::TitleAZ => list.sort_unstable_by(|a, b| a.file_name.cmp(&b.file_name)),
    SortMethod::TitleZA => list.sort_unstable_by(|a, b| b.file_name.cmp(&a.file_name)),
    SortMethod::NoSort => {}
  }

  let list_end = Instant::now();
  println!("list files took: {}ms", (list_end - list_start).as_millis());

  list
}

fn get_modified_time(dent: &DirEntry) -> SystemTime {
  match dent.metadata() {
    Ok(metadata) => metadata
      .modified()
      .expect("What to do if this doesn't work?"),
    Err(_) => panic!("I don't know what to do if we don't have metadata"),
  }
}

// fn grep_iter(patter: &str) -> Result

fn grep_life(pattern: &str, files: Vec<ListItem>) -> Result<Vec<ListItem>, Box<Error>> {
  let grep_start = Instant::now();

  //let's just bail if it's a super short search
  if pattern.len() < 2 {
    let grep_end = Instant::now();
    println!(
      "skipping grep took: {}ms",
      (grep_end - grep_start).as_millis()
    );
    return Ok(files);
  }

  let mut matches: Vec<ListItem> = vec![];
  let matcher = RegexMatcher::new(&pattern)?;
  let mut searcher = SearcherBuilder::new()
    .binary_detection(BinaryDetection::quit(b'\x00'))
    .build();

  for file in files {
    let result = searcher.search_path(
      &matcher,
      &file.path,
      UTF8(|lnum, line| {
        matches.push(ListItem {
          path: file.path.clone(),
          file_name: file.file_name.clone(),
          modified: file.modified,
          //we don't need the whole line, so this helps
          line: Some(line.chars().take(100).collect()),
          line_num: Some(lnum),
        });
        //we stop searching after our first find by returning false
        Ok(false)
      }),
    );
    if let Err(err) = result {
      eprintln!("{}: {}", file.path, err);
    }
  }

  let grep_end = Instant::now();
  println!("grep took: {}ms", (grep_end - grep_start).as_millis());
  Ok(matches)
}
