//! Byte transport to the scope: the WinUSB device (firmware usb_link v3, PID 5741, about 3x faster) or
//! the virtual COM port (PID 5740). Both carry the same protocol stream.

use crate::device::{PortChoice, USB_VID, WINUSB_PATH, WINUSB_PID};
use nusb::transfer::{Bulk, ControlOut, ControlType, In, Out, Recipient};
use nusb::MaybeFuture;
use std::io::{Read, Write};
use std::time::Duration;

const EP_IN: u8 = 0x81;
const EP_OUT: u8 = 0x02;
/// vendor request telling the firmware that the application is open (like DTR on the COM port)
const VREQ_HOST_OPEN: u8 = 0x22;

pub enum Conn {
    Serial(Box<dyn serialport::SerialPort>),
    Usb(UsbConn),
}

pub struct UsbConn {
    iface: nusb::Interface,
    reader: nusb::io::EndpointRead<Bulk>,
    writer: nusb::io::EndpointWrite<Bulk>,
}

impl UsbConn {
    fn host_open(&self, open: bool) -> Result<(), String> {
        self.iface
            .control_out(
                ControlOut {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Interface,
                    request: VREQ_HOST_OPEN,
                    value: open as u16,
                    index: self.iface.interface_number() as u16,
                    data: &[],
                },
                Duration::from_millis(500),
            )
            .wait()
            .map_err(|e| format!("WinUSB control request: {e}"))
    }
}

impl Drop for UsbConn {
    fn drop(&mut self) {
        let _ = self.host_open(false);
    }
}

/// Is a DSO-510 in WinUSB mode connected?
pub fn winusb_present() -> bool {
    nusb::list_devices().wait().map(|mut it| it.any(|d| d.vendor_id() == USB_VID && d.product_id() == WINUSB_PID)).unwrap_or(false)
}

fn open_usb() -> Result<UsbConn, String> {
    let di = nusb::list_devices()
        .wait()
        .map_err(|e| e.to_string())?
        .find(|d| d.vendor_id() == USB_VID && d.product_id() == WINUSB_PID)
        .ok_or("DSO-510 (WinUSB) not found")?;
    let dev = di.open().wait().map_err(|e| format!("WinUSB open: {e}"))?;
    let iface = dev.claim_interface(0).wait().map_err(|e| format!("WinUSB claim: {e}"))?;
    let reader = iface
        .endpoint::<Bulk, In>(EP_IN)
        .map_err(|e| e.to_string())?
        .reader(4096)
        .with_num_transfers(8)
        .with_read_timeout(Duration::from_millis(50));
    let writer = iface.endpoint::<Bulk, Out>(EP_OUT).map_err(|e| e.to_string())?.writer(64).with_num_transfers(2);
    let c = UsbConn { iface, reader, writer };
    c.host_open(true)?;
    Ok(c)
}

fn open_serial(p: &PortChoice) -> Result<Box<dyn serialport::SerialPort>, String> {
    let mut port = serialport::new(&p.path, 115_200)
        .timeout(Duration::from_millis(50))
        .open()
        .map_err(|e| format!("{}: {e}", p.label))?;
    // the firmware only streams while DTR is set (a terminal / app has the port open)
    port.write_data_terminal_ready(true).map_err(|e| e.to_string())?;
    Ok(port)
}

impl Conn {
    pub fn open(p: &PortChoice) -> Result<Conn, String> {
        if p.path == WINUSB_PATH { open_usb().map(Conn::Usb) } else { open_serial(p).map(Conn::Serial) }
    }

    /// Read what is there; Ok(0) / TimedOut when nothing arrived within ~50 ms.
    pub fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Conn::Serial(p) => p.read(buf),
            Conn::Usb(u) => u.reader.read(buf),
        }
    }

    pub fn write_all(&mut self, data: &[u8]) -> std::io::Result<()> {
        match self {
            Conn::Serial(p) => p.write_all(data),
            Conn::Usb(u) => {
                u.writer.write_all(data)?;
                u.writer.flush()
            }
        }
    }
}
