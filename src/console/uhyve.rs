use embedded_io::{ErrorType, Read, ReadReady, Write};
#[cfg(feature = "amd-sev")]
use uhyve_interface::v2::Hypercall;

use crate::errno::Errno;
use crate::uhyve::serial_buf_hypercall;
#[cfg(feature = "amd-sev")]
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
		// TODO: Implement serial_buf_hypercall on SEV
		#[cfg(feature = "amd-sev")]
		for &byte in buf {
			uhyve_hypercall(Hypercall::SerialWriteByte(byte));
		}
		#[cfg(not(feature = "amd-sev"))]
		serial_buf_hypercall(buf);
		Ok(buf.len())
	}

	fn flush(&mut self) -> Result<(), Self::Error> {
		Ok(())
	}
}
