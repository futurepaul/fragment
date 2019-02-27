use walkdir::{DirEntry, WalkDir};

use std::ffi::OsString;
use std::time::SystemTime;

#[derive(Clone)]
pub struct ListItem {
  pub path: String,
  pub file_name: String,
  pub modified: SystemTime,
}

pub fn list_of_all_files(root: &str) -> Vec<ListItem> {
  println!("gathering list of files from {}", &root);
  let dir = OsString::from(root);

  WalkDir::new(dir)
    .into_iter()
    //TODO: skipping dotfiles because I don't like
    //searching all the tantivy garbage
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
    })
    .collect::<Vec<ListItem>>()
}

fn get_modified_time(dent: &DirEntry) -> SystemTime {
  match dent.metadata() {
    Ok(metadata) => metadata
      .modified()
      .expect("What to do if this doesn't work?"),
    Err(_e) => panic!("I don't know what to do if we don't have metadata"),
  }
}

fn is_hidden(entry: &DirEntry) -> bool {
  entry
    .file_name()
    .to_str()
    .map(|s| s.starts_with('.'))
    .unwrap_or(false)
}
