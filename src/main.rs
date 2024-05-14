use std::{
    io::{self, Read, Seek},
    time::Instant,
};

use crossterm::event::{Event, KeyCode, KeyEventKind};

use crate::big_file_editor::{BigFileEditor, FileWindowFrame};

mod big_file_editor;
mod file;

fn main() -> io::Result<()> {
    crossterm::terminal::enable_raw_mode()?;

    run()?;

    crossterm::terminal::disable_raw_mode()?;
    Ok(())
}

fn run() -> io::Result<()> {
    let filename = "index.html";

    let start = Instant::now();
    let mut editor = BigFileEditor::from_path(filename).unwrap();
    println!("Read and indexed file in {}ms", start.elapsed().as_millis());

    let mut frame = FileWindowFrame {
        first_line: 0,
        first_column: 0,
        lines: 12,
        columns: 80,
    };

    read_and_print_window(&mut editor, &frame);

    loop {
        let event = crossterm::event::read()?;
        match event {
            Event::Key(e) if e.kind == KeyEventKind::Press => {
                match e.code {
                    KeyCode::Esc => break,
                    KeyCode::Left => {
                        frame.first_column = frame.first_column.saturating_sub(1);
                    },
                    KeyCode::Right => {
                        frame.first_column = frame.first_column.saturating_add(1);
                    },
                    KeyCode::Up => {
                        frame.first_line = frame.first_line.saturating_sub(1);
                    },
                    KeyCode::Down => {
                        frame.first_line = frame.first_line.saturating_add(1);
                    },
                    _ => continue,
                };

                read_and_print_window(&mut editor, &frame);
            },
            _ => {},
        }
    }
    Ok(())
}

fn read_and_print_window<T: Read + Seek>(editor: &mut BigFileEditor<T>, frame: &FileWindowFrame) {
    let window = editor.read_window(&frame);

    let mut output = String::new();
    output += "--------------------------------------------------------------------------------\n";
    for line in &window.lines {
        output += &format!("{}\n", line.replace("\n", "").replace("\r", ""));
    }
    output += "--------------------------------------------------------------------------------\n";
    print!("{}", output);
}
