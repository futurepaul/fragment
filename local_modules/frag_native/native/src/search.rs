use grep::regex::RegexMatcher;
use grep::searcher::sinks::UTF8;
use grep::searcher::{BinaryDetection, SearcherBuilder};
use walkdir::{DirEntry, WalkDir};

use std::error::Error;
use std::ffi::OsString;
use std::time::{Duration, Instant, SystemTime};

pub struct ListItem {
  pub path: String,
  pub file_name: String,
  pub line: String,
  pub line_num: u64,
}

struct FileItem {
  dent: DirEntry,
  modified: SystemTime,
  first_line: String,
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

fn list_of_all_files(root: &str, sort_by: SortMethod) -> Vec<FileItem> {
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
    .map(|dent| FileItem {
      dent: dent.clone(),
      modified: get_modified_time(&dent),
      //need to upgrade this to actually reading the first line of the file
      first_line: "".to_string(),
    })
    .collect::<Vec<FileItem>>();

  match sort_by {
    SortMethod::DateNewest => list.sort_unstable_by(|a, b| b.modified.cmp(&a.modified)),
    SortMethod::DateOldest => list.sort_unstable_by(|a, b| a.modified.cmp(&b.modified)),
    SortMethod::TitleAZ => {
      list.sort_unstable_by(|a, b| a.dent.file_name().cmp(&b.dent.file_name()))
    }
    SortMethod::TitleZA => {
      list.sort_unstable_by(|a, b| b.dent.file_name().cmp(&a.dent.file_name()))
    }
    SortMethod::NoSort => {}
  }

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

fn grep_life(pattern: &str, files: Vec<FileItem>) -> Result<Vec<ListItem>, Box<Error>> {
  let mut matches: Vec<ListItem> = vec![];
  let matcher = RegexMatcher::new(&pattern)?;
  let mut searcher = SearcherBuilder::new()
    .binary_detection(BinaryDetection::quit(b'\x00'))
    .build();

  for item in files {
    let result = searcher.search_path(
      &matcher,
      item.dent.path(),
      UTF8(|lnum, line| {
        let path = item.dent.path().display();
        matches.push(ListItem {
          path: path.to_string(),
          file_name: item.dent.file_name().to_os_string().into_string().unwrap(),
          //we don't need the whole line, so this helps
          line: line.chars().take(100).collect(),
          line_num: lnum,
        });
        //we stop searching after our first find by returning false
        Ok(false)
      }),
    );
    if let Err(err) = result {
      eprintln!("{}: {}", item.dent.path().display(), err);
    }
  }

  Ok(matches)
}
