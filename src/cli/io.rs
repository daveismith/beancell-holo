use core::fmt::{Error as FmtError, Result as FmtResult, Write as FmtWrite};
use core::convert::Infallible;

use esp_hal::Async;
use esp_hal::usb_serial_jtag::{UsbSerialJtagRx, UsbSerialJtagTx};
use embedded_io_async::{Read, Write};
use heapless::Vec;

const TX_CHUNK_SIZE: usize = 128;

fn is_newline(byte: u8) -> bool {
    byte == b'\r' || byte == b'\n'
}

fn is_newline_pair(first: u8, second: u8) -> bool {
    (first == b'\r' && second == b'\n') || (first == b'\n' && second == b'\r')
}

pub struct CliIo<'a> {
    rx: UsbSerialJtagRx<'a, Async>,
    tx: UsbSerialJtagTx<'a, Async>,
    pending_newline: Option<u8>,
}

impl<'a> CliIo<'a> {
    pub fn new(rx: UsbSerialJtagRx<'a, Async>, tx: UsbSerialJtagTx<'a, Async>) -> Self {
        Self {
            rx,
            tx,
            pending_newline: None,
        }
    }

    fn fill_normalized_chunk<const N: usize>(
        &mut self,
        input: &[u8],
        out: &mut Vec<u8, N>,
    ) -> usize {
        let mut consumed = 0;

        if let Some(pending) = self.pending_newline {
            if let Some(&first) = input.first() {
                if is_newline_pair(pending, first) {
                    if out.capacity() - out.len() < 2 {
                        return 0;
                    }

                    let _ = out.push(pending);
                    let _ = out.push(first);
                    self.pending_newline = None;
                    consumed = 1;
                } else {
                    if out.capacity() - out.len() < 2 {
                        return 0;
                    }

                    let _ = out.push(b'\r');
                    let _ = out.push(b'\n');
                    self.pending_newline = None;
                }
            } else {
                return 0;
            }
        }

        while consumed < input.len() {
            let byte = input[consumed];

            if is_newline(byte) {
                if consumed + 1 < input.len() {
                    let next = input[consumed + 1];
                    if is_newline_pair(byte, next) {
                        if out.capacity() - out.len() < 2 {
                            break;
                        }
                        let _ = out.push(byte);
                        let _ = out.push(next);
                        consumed += 2;
                        continue;
                    }

                    if out.capacity() - out.len() < 2 {
                        break;
                    }
                    let _ = out.push(b'\r');
                    let _ = out.push(b'\n');
                    consumed += 1;
                    continue;
                }

                self.pending_newline = Some(byte);
                consumed += 1;
                break;
            }

            if out.push(byte).is_err() {
                break;
            }
            consumed += 1;
        }

        consumed
    }

    async fn write_normalized_async(&mut self, buf: &[u8]) -> Result<(), Infallible> {
        let mut index = 0;

        while index < buf.len() {
            let mut chunk: Vec<u8, TX_CHUNK_SIZE> = Vec::new();
            let consumed = self.fill_normalized_chunk(&buf[index..], &mut chunk);

            if consumed == 0 && chunk.is_empty() {
                break;
            }

            if !chunk.is_empty() {
                embedded_io_async::Write::write_all(&mut self.tx, chunk.as_slice()).await?;
            }

            index += consumed;
        }

        Ok(())
    }

    fn write_normalized_blocking(&mut self, buf: &[u8]) -> Result<(), Infallible> {
        let mut index = 0;

        while index < buf.len() {
            let mut chunk: Vec<u8, TX_CHUNK_SIZE> = Vec::new();
            let consumed = self.fill_normalized_chunk(&buf[index..], &mut chunk);

            if consumed == 0 && chunk.is_empty() {
                break;
            }

            if !chunk.is_empty() {
                self.tx.write(chunk.as_slice())?;
            }

            index += consumed;
        }

        Ok(())
    }

    async fn flush_pending_newline(&mut self) -> Result<(), Infallible> {
        if self.pending_newline.take().is_some() {
            embedded_io_async::Write::write_all(&mut self.tx, b"\r\n").await?;
        }
        Ok(())
    }
}

impl embedded_io_async::ErrorType for CliIo<'_> {
    type Error = Infallible;
}

impl Read for CliIo<'_> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        embedded_io_async::Read::read(&mut self.rx, buf).await
    }
}

impl Write for CliIo<'_> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.write_normalized_async(buf).await?;
        Ok(buf.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.flush_pending_newline().await?;
        embedded_io_async::Write::flush(&mut self.tx).await
    }
}

impl FmtWrite for CliIo<'_> {
    fn write_str(&mut self, s: &str) -> FmtResult {
        self.write_normalized_blocking(s.as_bytes())
            .map_err(|_| FmtError)
    }
}
