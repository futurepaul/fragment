use ignore::Walk;
use std::error::Error;
use std::fs;
use std::io::BufRead;
use std::io::BufReader;
use std::path::Path;
use std::path::PathBuf;
// use tantivy::collector::TopDocs;
// use tantivy::query::QueryParser;
// use tantivy::schema::{Schema, STORED, TEXT};
// use tantivy::Document;
// use tantivy::Index;

use std::time::{Duration, Instant, SystemTime};

use grep::regex::RegexMatcher;
use grep::searcher::sinks::UTF8;
use grep::searcher::{BinaryDetection, SearcherBuilder};

use walkdir::{DirEntry, WalkDir};

use std::ffi::OsString;

#[derive(Clone, Debug)]
pub struct ListItem {
  pub path: String,
  pub file_name: String,
  pub modified: SystemTime,
  pub first_line: String,
}

pub enum SortMethod {
  DateNewest,
  DateOldest,
  TitleAZ,
  TitleZA,
  NoSort,
}

pub fn search(pattern: &str, dir: &str) -> Result<Vec<ListItem>, Box<Error>> {
  // let root = "./node_modules";
  // let root = "../notes_grep_test";
  let files = list_of_all_files(dir, SortMethod::DateNewest);

  grep_life(pattern, &files)
}

pub fn list_of_all_files(root: &str, sort_by: SortMethod) -> Vec<ListItem> {
  let list_start = Instant::now();
  // println!("gathering list of files from {}", &root);
  let dir = OsString::from(root);

  let mut list = Vec::new();

  for result in Walk::new(dir) {
    match result {
      Ok(entry) => {
        if entry.file_type().unwrap().is_file() {
          list.push(ListItem {
            path: entry.path().display().to_string(),
            file_name: entry.file_name().to_os_string().into_string().unwrap(),
            modified: get_modified_time_from_path(&entry.path().display().to_string()),
            first_line: ("hello".to_string()),
          })
        }
      }
      Err(err) => println!("WALKDIR ERROR: {}", err),
    }
  }

  match sort_by {
    SortMethod::DateNewest => list.sort_unstable_by(|a, b| b.modified.cmp(&a.modified)),
    SortMethod::DateOldest => list.sort_unstable_by(|a, b| a.modified.cmp(&b.modified)),
    SortMethod::TitleAZ => list.sort_unstable_by(|a, b| a.file_name.cmp(&b.file_name)),
    SortMethod::TitleZA => list.sort_unstable_by(|a, b| b.file_name.cmp(&a.file_name)),
    SortMethod::NoSort => {}
  }

  // dbg!(list[1].clone());
  let list_end = Instant::now();
  println!("list files took: {}ms", (list_end - list_start).as_millis());

  list
}

fn get_modified_time_from_path(path: &str) -> SystemTime {
  match Path::new(path).metadata() {
    Ok(metadata) => metadata
      .modified()
      .expect("What to do if this doesn't work?"),
    Err(_e) => panic!("I don't know what to do if we don't have metadata"),
  }
}

pub fn grep_life(pattern: &str, files: &Vec<ListItem>) -> Result<Vec<ListItem>, Box<Error>> {
  let grep_start = Instant::now();

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
          first_line: file.first_line.clone(),
        });
        //we stop searching after our first find by returning false
        Ok(false)
      }),
    );
    if let Err(err) = result {
      println!("GREP ERROR: {}: {}", file.path, err);
    }
  }

  let grep_end = Instant::now();
  println!("grep took: {}ms", (grep_end - grep_start).as_millis());
  Ok(matches)
}
