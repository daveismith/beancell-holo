use core::convert::Infallible;
use core::fmt::{Error as FmtError, Result as FmtResult, Write as FmtWrite};

use embassy_time::Timer;
use embedded_io_async::{Read, Write};
use esp_hal::Async;
use esp_hal::uart::{UartRx, UartTx};
use esp_hal::usb_serial_jtag::{UsbSerialJtagRx, UsbSerialJtagTx};
use heapless::Vec;

const TX_CHUNK_SIZE: usize = 128;

fn is_newline(byte: u8) -> bool {
    byte == b'\r' || byte == b'\n'
}

fn is_newline_pair(first: u8, second: u8) -> bool {
    (first == b'\r' && second == b'\n') || (first == b'\n' && second == b'\r')
}

struct NewlineNormalizer {
    pending_newline: Option<u8>,
}

impl NewlineNormalizer {
    const fn new() -> Self {
        Self {
            pending_newline: None,
        }
    }

    fn fill_normalized_chunk<const N: usize>(&mut self, input: &[u8], out: &mut Vec<u8, N>) -> usize {
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

    fn write_normalized_blocking<E>(
        &mut self,
        buf: &[u8],
        mut sink: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<(), E> {
        let mut index = 0;

        while index < buf.len() {
            let mut chunk: Vec<u8, TX_CHUNK_SIZE> = Vec::new();
            let consumed = self.fill_normalized_chunk(&buf[index..], &mut chunk);

            if consumed == 0 && chunk.is_empty() {
                break;
            }

            if !chunk.is_empty() {
                sink(chunk.as_slice())?;
            }

            index += consumed;
        }

        Ok(())
    }
}

pub struct UsbCliIo<'a> {
    rx: UsbSerialJtagRx<'a, Async>,
    tx: UsbSerialJtagTx<'a, Async>,
    normalizer: NewlineNormalizer,
}

impl<'a> UsbCliIo<'a> {
    pub fn new(rx: UsbSerialJtagRx<'a, Async>, tx: UsbSerialJtagTx<'a, Async>) -> Self {
        Self {
            rx,
            tx,
            normalizer: NewlineNormalizer::new(),
        }
    }
    async fn flush_pending_newline(&mut self) -> Result<(), Infallible> {
        if self.normalizer.pending_newline.take().is_some() {
            embedded_io_async::Write::write_all(&mut self.tx, b"\r\n").await?;
        }
        Ok(())
    }
}

impl embedded_io_async::ErrorType for UsbCliIo<'_> {
    type Error = Infallible;
}

impl Read for UsbCliIo<'_> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        loop {
            let len = embedded_io_async::Read::read(&mut self.rx, buf).await?;
            if len > 0 {
                return Ok(len);
            }
            Timer::after_millis(1).await;
        }
    }
}

impl Write for UsbCliIo<'_> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        embedded_io_async::Write::write_all(&mut self.tx, buf).await?;
        Ok(buf.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.flush_pending_newline().await?;
        embedded_io_async::Write::flush(&mut self.tx).await
    }
}

impl FmtWrite for UsbCliIo<'_> {
    fn write_str(&mut self, s: &str) -> FmtResult {
        self.normalizer
            .write_normalized_blocking(s.as_bytes(), |chunk| self.tx.write(chunk))
            .map_err(|_| FmtError)
    }
}

pub struct UartCliIo<'a> {
    rx: UartRx<'a, Async>,
    tx: UartTx<'a, Async>,
    normalizer: NewlineNormalizer,
}

impl<'a> UartCliIo<'a> {
    pub fn new(rx: UartRx<'a, Async>, tx: UartTx<'a, Async>) -> Self {
        Self {
            rx,
            tx,
            normalizer: NewlineNormalizer::new(),
        }
    }

    async fn flush_pending_newline(&mut self) -> Result<(), embedded_io_async::ErrorKind> {
        if self.normalizer.pending_newline.take().is_some() {
            embedded_io_async::Write::write_all(&mut self.tx, b"\r\n")
                .await
                .map_err(|_| embedded_io_async::ErrorKind::Other)?;
        }
        Ok(())
    }
}

impl embedded_io_async::ErrorType for UartCliIo<'_> {
    type Error = embedded_io_async::ErrorKind;
}

impl Read for UartCliIo<'_> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        loop {
            let len = embedded_io_async::Read::read(&mut self.rx, buf)
                .await
                .map_err(|_| embedded_io_async::ErrorKind::Other)?;
            if len > 0 {
                return Ok(len);
            }
            Timer::after_millis(1).await;
        }
    }
}

impl Write for UartCliIo<'_> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        embedded_io_async::Write::write_all(&mut self.tx, buf)
            .await
            .map_err(|_| embedded_io_async::ErrorKind::Other)?;
        Ok(buf.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.flush_pending_newline().await?;
        embedded_io_async::Write::flush(&mut self.tx)
            .await
            .map_err(|_| embedded_io_async::ErrorKind::Other)
    }
}

impl FmtWrite for UartCliIo<'_> {
    fn write_str(&mut self, s: &str) -> FmtResult {
        self.normalizer
            .write_normalized_blocking(s.as_bytes(), |chunk| self.tx.write(chunk).map(|_| ()))
            .map_err(|_| FmtError)
    }
}
