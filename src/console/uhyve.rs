#[cfg(all(feature = "amd-sev", feature = "uhyve"))]
use core::fmt;

use embedded_io::{ErrorType, Read, ReadReady, Write};
#[cfg(all(feature = "amd-sev", feature = "uhyve"))]
use uhyve_interface::v2::Hypercall;

use crate::errno::Errno;
use crate::uhyve::serial_buf_hypercall;
#[cfg(all(feature = "amd-sev", feature = "uhyve"))]
use crate::uhyve::uhyve_hypercall;

pub(crate) struct UhyveSerial;

impl UhyveSerial {
	pub const fn new() -> Self {
		Self {}
	}
}

impl ErrorType for UhyveSerial {
	type Error = Errno;
}

impl Read for UhyveSerial {
	fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
		let _ = buf;
		Ok(0)
	}
}

impl ReadReady for UhyveSerial {
	fn read_ready(&mut self) -> Result<bool, Self::Error> {
		Ok(false)
	}
}

impl Write for UhyveSerial {
	fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
		serial_buf_hypercall(buf);
		Ok(buf.len())
	}

	fn flush(&mut self) -> Result<(), Self::Error> {
		Ok(())
	}
}

/// Panic output for uhyve.
#[cfg(all(feature = "amd-sev", feature = "uhyve"))]
struct PanicWriter;

#[cfg(all(feature = "amd-sev", feature = "uhyve"))]
impl fmt::Write for PanicWriter {
	fn write_str(&mut self, s: &str) -> fmt::Result {
		for &byte in s.as_bytes() {
			uhyve_hypercall(Hypercall::SerialWriteByte(byte));
		}
		Ok(())
	}
}

#[cfg(all(feature = "amd-sev", feature = "uhyve"))]
pub(crate) fn panic_print(args: fmt::Arguments<'_>) {
	let _ = fmt::Write::write_fmt(&mut PanicWriter, args);
}
