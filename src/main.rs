use std::{io::Read, time::Instant};

#[derive(Debug)]
struct LineData {
    line_start: usize,
    line_length: usize,
    line_display_length: usize,
}

// fn index_lines_chars(s: &str) -> Vec<LineData> {
//     let mut lines_data = vec![];

//     let mut i = 0;
//     for c in s.chars() {
//         if c == '\n' {
//             lines_data.push(i);
//         }
//         i += 1;
//     }

//     lines_data
// }

fn index_lines_bytes(s: &str) -> Vec<LineData> {
    let mut lines_data = vec![];

    let mut line_start = 0;
    let mut line_length = 0;
    let mut line_display_length = 0;
    let mut cr = false;

    for &c in s.as_bytes() {
        line_length += 1;

        if c > 127 {
            panic!("non-ascii found ({})", c);
        } else if c == 9 {
            /* TAB (\t) */
            line_display_length += 8;
        } else if c == 13 {
            /* CR (\r) */
            cr = true;
        } else if c == 10 {
            /* NL (\n) */
            if cr {
                lines_data.push(LineData {
                    line_start,
                    line_length,
                    line_display_length,
                });
                line_start += line_length;
            }
            cr = false;
        } else if c >= 32 && c != 127 {
            line_display_length += 1;
        }
    }

    lines_data
}

// fn read_and_index_chars(filename: &str) -> Vec<LineData> {
//     let start = Instant::now();
//     let contents = std::fs::read_to_string(filename).unwrap();
//     println!("Read file in {}ms", start.elapsed().as_millis());

//     let start = Instant::now();
//     let indices = index_lines_chars(&contents);
//     println!("Indexed file in {}ms", start.elapsed().as_millis());

//     indices
// }

fn read_and_index_bytes(filename: &str) -> Vec<LineData> {
    let start = Instant::now();
    let contents = std::fs::read_to_string(filename).unwrap();
    println!("Read file in {}ms", start.elapsed().as_millis());

    let start = Instant::now();
    let indices = index_lines_bytes(&contents);
    println!("Indexed file in {}ms", start.elapsed().as_millis());

    indices
}

// fn read_and_index_buf_reader(filename: &str) -> Vec<LineData> {
//     let file = std::fs::File::open(filename).unwrap();
//     let mut reader = std::io::BufReader::new(file);

//     let mut buf = vec![0; 64 * 1024 * 1024];
//     let mut indices = vec![];
//     let mut cr = false;
//     while let Ok(nread) = reader.read(&mut buf) {
//         if nread == 0 {
//             break;
//         }

//         for i in 0..nread as u32 {
//             let c = buf[i as usize];

//             if c == 13 {
//                 cr = true;
//             } else if cr {
//                 if c == 10 {
//                     indices.push(i);
//                 }
//                 cr = false;
//             }
//         }
//     }

//     indices
// }

fn main() {
    let start = Instant::now();
    let filename = "index.html";
    // let indices = read_and_index_chars(filename);
    let indices = read_and_index_bytes(filename);
    // let indices = read_and_index_buf_reader(filename);

    println!("Read and indexed file in {}ms", start.elapsed().as_millis());
    for line in &indices[..indices.len().min(128)] {
        println!("{:?}", line);
    }
}
