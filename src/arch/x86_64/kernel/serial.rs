use alloc::collections::VecDeque;
#[cfg(feature = "amd-sev")]
use crate::arch::kernel::amd_sev;

use embedded_io::{ErrorType, Read, ReadReady, Write};
use hermit_sync::{InterruptTicketMutex, Lazy};
use uart_16550::backend::PioBackend;
use uart_16550::spec::registers::IER;
use uart_16550::{Config, Uart16550};
#[cfg(feature = "amd-sev")]
use ghcb::serial::SevPanicPort;

#[cfg(feature = "amd-sev")]
use crate::arch::kernel::amd_sev::allocations::ghcb::EmergencyChannelManager;
#[cfg(feature = "pci")]
use crate::drivers::InterruptHandlerMap;
use crate::errno::Errno;

#[cfg(feature = "pci")]
const SERIAL_IRQ: u8 = 4;

const SERIAL_PORT_BASE: u16 = 0x3f8;

#[cfg(feature = "amd-sev")]
static PANIC_PORT: Lazy<InterruptTicketMutex<SevPanicPort<EmergencyChannelManager>>> = Lazy::new(|| unsafe {
	InterruptTicketMutex::new(SevPanicPort::new(SERIAL_PORT_BASE))
});

#[cfg(feature = "amd-sev")]
pub fn get_panic_port() -> &'static InterruptTicketMutex<SevPanicPort<EmergencyChannelManager>> {
	Lazy::force(&PANIC_PORT)
}

/// Panic exit early in boot.
/// This initializes a "raw" console, prints the message, and kills the guest immediately.
pub fn early_panic(msg: &str) -> ! {
	let mut port = unsafe {
		Uart16550::<PioBackend>::new_port(SERIAL_PORT_BASE).unwrap()
	};

	let _ = port.init(Config::default());
	let _ = port.write_all(msg.as_bytes());
	let _ = port.write("\n".as_bytes());
	crate::arch::kernel::processor::triple_fault();
}

static UART_DEVICE: Lazy<InterruptTicketMutex<UartDevice>> =
	Lazy::new(|| unsafe { InterruptTicketMutex::new(UartDevice::new()) });

struct UartDevice {
	#[cfg(not(feature = "amd-sev"))]
	pub uart: Uart16550<PioBackend>,
	#[cfg(feature = "amd-sev")]
	pub uart: amd_sev::paravirt_uart::SerialPort,
	pub buffer: VecDeque<u8>,
}

impl UartDevice {
	pub unsafe fn new() -> Self {
		let base_port = SERIAL_PORT_BASE;
		let mut uart = unsafe {
			cfg_select! {
				feature = "amd-sev" => amd_sev::paravirt_uart::SerialPort::new(base_port),
				_ => Uart16550::new_port(base_port).unwrap(),
			}
		};
		let config = Config {
			interrupts: IER::DATA_READY,
			..Default::default()
		};
		uart.init(config).ok();
		// Once we have a fallback destination for output,
		// we should log any error above and run
		// `test_loopback` and `check_connected` here.

		Self {
			uart,
			buffer: VecDeque::new(),
		}
	}
}

pub(crate) struct SerialDevice;

impl SerialDevice {
	pub fn new() -> Self {
		Self {}
	}
}

impl ErrorType for SerialDevice {
	type Error = Errno;
}

impl Read for SerialDevice {
	fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
		Ok(UART_DEVICE.lock().buffer.read(buf)?)
	}
}

impl ReadReady for SerialDevice {
	fn read_ready(&mut self) -> Result<bool, Self::Error> {
		Ok(!UART_DEVICE.lock().buffer.is_empty())
	}
}

impl Write for SerialDevice {
	fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
		let mut guard = UART_DEVICE.lock();
		let n = guard.uart.write(buf)?;
		Ok(n)
	}

	fn flush(&mut self) -> Result<(), Self::Error> {
		Ok(())
	}
}

#[cfg(feature = "pci")]
pub(crate) fn handle_interrupt() {
	let mut guard = UART_DEVICE.lock();

	while guard.uart.read_ready().unwrap() {
		let mut buf = [0; 256];
		let n = guard.uart.read(&mut buf).unwrap();
		guard.buffer.write_all(&buf[..n]).unwrap();
	}

	drop(guard);
	crate::console::CONSOLE_WAKER.lock().wake();
}

#[cfg(feature = "pci")]
pub(crate) fn register_handler(handlers: &mut InterruptHandlerMap) {
	super::interrupts::add_irq_name(SERIAL_IRQ, "COM1");
	handlers
		.entry(SERIAL_IRQ)
		.or_default()
		.push_back(handle_interrupt);
}
