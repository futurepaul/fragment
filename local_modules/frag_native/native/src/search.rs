use grep::regex::RegexMatcher;
use grep::searcher::sinks::UTF8;
use grep::searcher::{BinaryDetection, SearcherBuilder};
use walkdir::{DirEntry, WalkDir};

use std::error::Error;
use std::ffi::OsString;
use std::time::{Duration, Instant};

pub struct ListItem {
  pub path: String,
  pub file_name: String,
  pub line: String,
  pub line_num: u64,
}

fn is_hidden(entry: &DirEntry) -> bool {
  entry
    .file_name()
    .to_str()
    .map(|s| s.starts_with('.'))
    .unwrap_or(false)
}

pub fn grep_life(pattern: &str) -> Result<Vec<ListItem>, Box<Error>> {
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
        let path = dent.path().display();
        // let trimmed_path = path.trim_start_matches('"').trim_end_matches('"');
        matches.push(ListItem {
          path: path.to_string(),
          file_name: dent.file_name().to_os_string().into_string().unwrap(),
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
