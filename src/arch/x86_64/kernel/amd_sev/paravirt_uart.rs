use embedded_io::{Read, Write};
use uart_16550::Config;
use ghcb::serial::SevSerialPort;
use crate::arch::kernel::amd_sev::StaticGhcbManager;
use crate::errno::Errno;


#[derive(Debug)]
#[repr(transparent)]
pub struct SerialPort(SevSerialPort<StaticGhcbManager>);

impl SerialPort {
	pub const unsafe fn new(base: u16) -> Self {
		Self(SevSerialPort::new(base))
	}

	pub fn init(&mut self, c: Config) -> Result<(), Errno> {
		self.0.init(!c.interrupts.is_empty());
		Ok(())
	}

	pub fn write(&mut self, data: &[u8]) -> Result<usize, Errno> {
		self.0.write(data).map_err(|_| Errno::Again)
	}

	pub fn read_ready(&mut self) -> Result<bool, Errno> {
		Ok(self.0.read_ready())
	}

	pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, Errno> {
		self.0.read(buf).map_err(|_| Errno::Again)
	}
}