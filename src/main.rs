use std::{
    cmp::Ordering,
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    time::Instant,
};

use file::ReadRetry;

use crate::big_file_editor::{BigFileEditor, FileWindowFrame};

mod big_file_editor;
mod file;

enum EolSequence {
    LF,
    CRLF,
}

fn main() {
    let start = Instant::now();
    let filename = "1gb.txt";
    let mut editor = BigFileEditor::from_path(filename).unwrap();

    println!("Read and indexed file in {}ms", start.elapsed().as_millis());
    // println!("{:#?}", editor);

    let start = Instant::now();
    let window = editor.read_window(&FileWindowFrame {
        first_line: 181,
        first_column: 2,
        lines: 8,
        columns: 80,
    });
    println!("Read window in {}ms", start.elapsed().as_millis());

    println!("Window:");
    println!("--------------------------------------------------------------------------------");
    for line in &window.lines {
        println!("|{}", line);
    }
    println!("--------------------------------------------------------------------------------");
}
