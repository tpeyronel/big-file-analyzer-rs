use std::io::{self, Read};

pub trait ReadRetry {
    fn read_with_retry(&mut self, buf: &mut [u8]) -> io::Result<usize>;
}

impl<T: Read> ReadRetry for T {
    fn read_with_retry(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut bytes_read = 0;
        while bytes_read < buf.len() {
            match self.read(&mut buf[bytes_read..]) {
                Ok(bytes) => {
                    if bytes == 0 {
                        break;
                    }

                    bytes_read += bytes;
                },
                Err(err) => match err.kind() {
                    io::ErrorKind::Interrupted => continue,
                    _ => return Err(err),
                },
            }
        }

        Ok(bytes_read)
    }
}
