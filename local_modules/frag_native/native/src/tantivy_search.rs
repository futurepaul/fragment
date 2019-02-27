use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{Schema, STORED, TEXT};
use tantivy::Document;
use tantivy::Index;

use walkdir::{DirEntry, WalkDir};

use std::ffi::OsString;

// fn main() {
//   let index_storage_path = PathBuf::from("test/.index_storage");
//   let (index, how_many_indexed) = build_index("test", index_storage_path).unwrap();
//   println!("indexed {} documents!", how_many_indexed);
//   search(index, "hey").expect("search didn't work hmm");
// }

#[derive(Clone)]
pub struct ListItem {
  pub path: String,
  pub file_name: String,
  pub modified: SystemTime,
}

fn list_of_all_files(root: &str) -> Vec<ListItem> {
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

fn get_modified_time_from_path(path: &str) -> SystemTime {
  match Path::new(path).metadata() {
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

pub fn create_schema() -> Schema {
  let mut schema_builder = Schema::builder();
  schema_builder.add_text_field("title", TEXT | STORED);
  schema_builder.add_text_field("body", TEXT);
  schema_builder.build()
}

pub fn build_index(search_path: &str, index_path: PathBuf) -> tantivy::Result<(Index, u64)> {
  let schema = create_schema();

  //return early if we already have an index
  let index = match Index::open_in_dir(&index_path) {
    Ok(index) => {
      println!("opening index...");
      return Ok((index, 0));
    }
    Err(_) => {
      println!("creating index...");
      Index::create_in_dir(&index_path, schema.clone())?
    }
  };

  let files = list_of_all_files(search_path);

  let schema = create_schema();

  let mut index_writer = index.writer(50_000_000)?;

  let title = schema.get_field("title").unwrap();
  let body = schema.get_field("body").unwrap();

  for file in files {
    // println!("{}", file.path);
    let mut doc = Document::default();
    doc.add_text(title, &file.path);
    //TODO: this might be slower than a buffer? idk.
    match &fs::read_to_string(&file.path) {
      Ok(file_content) => doc.add_text(body, file_content),
      Err(e) => {
        eprintln!("Couldn't index {} because: {}", &file.path, e);
        continue;
      }
    }

    index_writer.add_document(doc);
  }

  match index_writer.commit() {
    Ok(how_many) => {
      println!("got to the ok branch of index_writer.commit");
      index_writer.wait_merging_threads()?;
      Ok((index, how_many))
    }
    Err(e) => {
      eprintln!("Error during indexing: {}", e);
      Err(e)
    }
  }
}

pub fn search(index: Index, query: &str) -> tantivy::Result<Vec<ListItem>> {
  index.load_searchers()?;
  let searcher = index.searcher();

  //TODO: duplicate schema effort
  let schema = create_schema();
  let title = schema.get_field("title").unwrap();
  let body = schema.get_field("body").unwrap();

  let query_parser = QueryParser::for_index(&index, vec![title, body]);

  let query = query_parser.parse_query(query)?;

  //TODO: next version of tantivy has a TopCollector instead
  let top_docs = searcher.search(&*query, &TopDocs::with_limit(100))?;

  let mut matches: Vec<ListItem> = vec![];

  for (_score, doc_address) in top_docs {
    let retrieved_doc = searcher.doc(doc_address)?;
    //second unwrap is because text returns a Some
    let path = retrieved_doc.get_first(title).unwrap().text().unwrap();
    matches.push(ListItem {
      path: path.to_string(),
      //lol this is so gross TODO
      file_name: Path::new(path)
        .file_name()
        .unwrap()
        .to_os_string()
        .into_string()
        .unwrap(),
      modified: get_modified_time_from_path(path),
    })
  }

  Ok(matches)
}
