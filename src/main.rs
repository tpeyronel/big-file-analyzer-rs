use std::{
    io::{self, Read, Seek, Write},
    time::Instant,
};

use crossterm::{
    cursor,
    event::{Event, KeyCode, KeyEventKind},
    queue, style,
    terminal::{self, ClearType},
};

use crate::big_file_editor::{BigFileEditor, FileWindowFrame, TAB_SIZE};

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
        lines: crossterm::terminal::size()?.1.saturating_sub(2) as usize,
        columns: 80,
    };

    let mut stdout = std::io::stdout();

    read_and_print_window(&mut stdout, &mut editor, &frame)?;

    loop {
        let event = crossterm::event::read()?;
        match event {
            Event::Key(e) if e.kind == KeyEventKind::Press => {
                match e.code {
                    KeyCode::Esc | KeyCode::Char('q') => break,
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

                read_and_print_window(&mut stdout, &mut editor, &frame)?;
            },
            Event::Resize(_width, height) => {
                frame.lines = height.saturating_sub(2) as usize;
                read_and_print_window(&mut stdout, &mut editor, &frame)?;
            },
            _ => {},
        }
    }
    Ok(())
}

fn read_and_print_window(
    stdout: &mut io::Stdout,
    editor: &mut BigFileEditor,
    frame: &FileWindowFrame,
) -> io::Result<()> {
    queue!(
        stdout,
        style::ResetColor,
        terminal::Clear(ClearType::All),
        cursor::Hide,
        cursor::MoveTo(1, 1)
    )?;

    let window = editor.read_window(&frame);
    let offset = frame.first_column - window.frame.first_column;

    let mut output = String::new();
    output += "--------------------------------------------------------------------------------\n";
    for line in &window.lines {
        let line = line.replace("\n", "").replace("\r", "");
        let mut line_without_tabs = String::new();
        let mut column = window.frame.first_column;
        for c in line.chars() {
            if c == '\t' {
                let tabs = TAB_SIZE - (column % TAB_SIZE);
                line_without_tabs += &" ".repeat(tabs);
                column += TAB_SIZE - (column % TAB_SIZE);
            } else {
                line_without_tabs.push(c);
                column += 1;
                continue;
            }
        }
        if line_without_tabs.len() > offset {
            output += &format!("{}\n", &line_without_tabs[offset..]);
        } else {
            output += &format!("\n");
        }
    }
    output += "--------------------------------------------------------------------------------";
    print!("{}", output);
    stdout.flush()?;

    Ok(())
}
