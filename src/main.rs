use std::{
    io::{self, Write},
    time::Instant,
};

use crossterm::{
    cursor,
    event::{Event, EventStream, KeyCode, KeyEventKind},
    queue, style,
    terminal::{self, ClearType},
};
use file_analyzer::big_file_editor::{BigFileEditor, FileWindow, FileWindowFrame, TAB_SIZE};

use std::{io::stdout, time::Duration};

use futures::{future::FutureExt, select, StreamExt};

#[tokio::main]
async fn main() -> io::Result<()> {
    // crossterm::terminal::enable_raw_mode()?;

    run().await?;

    // crossterm::terminal::disable_raw_mode()?;
    Ok(())
}

async fn run() -> io::Result<()> {
    let filename = "rnd_very_wide.txt";
    // let filename = "rnd_wide.txt";
    // let filename = "index.html";

    let start = Instant::now();
    let mut editor = BigFileEditor::from_path(filename).unwrap();

    let mut frame = FileWindowFrame {
        first_line: 0,
        first_column: 0,
        lines: crossterm::terminal::size()?.1.saturating_sub(2) as usize,
        columns: 80,
    };

    let (observer_configurator, mut observer) = editor.subscribe_window(frame.clone());

    let mut stdout = std::io::stdout();

    let window = observer.read_window().await.unwrap();
    print_window(&mut stdout, &window, &frame)?;

    let mut reader = EventStream::new();

    loop {
        let mut event = reader.next().fuse();

        select! {
            maybe_event = event => {
                match maybe_event {
                    Some(Ok(event)) => {
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

                                observer_configurator.set_frame(frame.clone());
                            },
                            Event::Resize(_width, height) => {
                                frame.lines = height.saturating_sub(2) as usize;

                                observer_configurator.set_frame(frame.clone());
                            },
                            _ => {},
                        }
                    }
                    Some(Err(e)) => println!("Error: {:?}\r", e),
                    None => break,
                }
            }
            window = observer.read_window().fuse() => {
                print_window(&mut stdout, &window.as_ref().unwrap(), &frame)?;
            },
        };
    }

    // let mut window_fut = Box::pin(subscriber.read_window().fuse());

    // loop {
    //     let mut event = reader.next().fuse();

    //     select! {
    //         maybe_event = event => {
    //             match maybe_event {
    //                 Some(Ok(event)) => {
    //                     match event {
    //                         Event::Key(e) if e.kind == KeyEventKind::Press => {
    //                             match e.code {
    //                                 KeyCode::Esc | KeyCode::Char('q') => break,
    //                                 KeyCode::Left => {
    //                                     frame.first_column = frame.first_column.saturating_sub(1);
    //                                 },
    //                                 KeyCode::Right => {
    //                                     frame.first_column = frame.first_column.saturating_add(1);
    //                                 },
    //                                 KeyCode::Up => {
    //                                     frame.first_line = frame.first_line.saturating_sub(1);
    //                                 },
    //                                 KeyCode::Down => {
    //                                     frame.first_line = frame.first_line.saturating_add(1);
    //                                 },
    //                                 _ => continue,
    //                             };

    //                             subscriber.set_frame(frame.clone()).await;
    //                         },
    //                         Event::Resize(_width, height) => {
    //                             frame.lines = height.saturating_sub(2) as usize;

    //                             subscriber.set_frame(frame.clone()).await;
    //                         },
    //                         _ => {},
    //                     }
    //                 }
    //                 Some(Err(e)) => println!("Error: {:?}\r", e),
    //                 None => break,
    //             }
    //         }
    //         window = window_fut => {
    //             print_window(&mut stdout, &window.as_ref().unwrap(), &frame)?;

    //             drop(window_fut);
    //             window_fut = Box::pin(subscriber.read_window().fuse());
    //         },
    //     };
    // }

    Ok(())
}

fn print_window(
    stdout: &mut io::Stdout,
    window: &FileWindow,
    frame: &FileWindowFrame,
) -> io::Result<()> {
    // queue!(
    //     stdout,
    //     style::ResetColor,
    //     terminal::Clear(ClearType::All),
    //     cursor::Hide,
    //     cursor::MoveTo(1, 1)
    // )?;

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
