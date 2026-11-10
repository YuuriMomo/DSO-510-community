//! Finding the DSO-510.
//!
//! The scope is opened through its COM-port *device interface path*
//! (`\\?\USB#VID_2E3C&PID_5740#<serial>#{86e0d1e0-...}`), not through its `COMx` name. The COM
//! name can be shadowed by other drivers (e.g. com0com virtual port pairs that claim the same
//! number), the interface path cannot.

pub const USB_VID: u16 = 0x2E3C;
pub const USB_PID: u16 = 0x5740;
/// The same firmware as a WinUSB device (default since usb_link v3; long press of button 0 switches).
pub const WINUSB_PID: u16 = 0x5741;
/// `PortChoice::path` of the WinUSB device.
pub const WINUSB_PATH: &str = "winusb:dso510";

#[derive(Clone, Debug, PartialEq)]
pub struct PortChoice {
    /// What the user sees in the port list.
    pub label: String,
    /// What is passed to the serial port driver.
    pub path: String,
    /// True for the DSO-510 itself.
    pub is_dso: bool,
}

/// All candidate ports: the WinUSB DSO-510, the DSO-510 COM interfaces, then every other serial port.
pub fn list() -> Vec<PortChoice> {
    let mut v: Vec<PortChoice> = Vec::new();
    if crate::transport::winusb_present() {
        v.push(PortChoice { label: "DSO-510 (WinUSB)".into(), path: WINUSB_PATH.into(), is_dso: true });
    }
    v.extend(dso_interfaces()
        .into_iter()
        .map(|(path, serial)| PortChoice { label: format!("DSO-510 COM ({serial})"), path, is_dso: true }));
    for p in serialport::available_ports().unwrap_or_default() {
        let is_dso = matches!(&p.port_type,
            serialport::SerialPortType::UsbPort(u) if u.vid == USB_VID && u.pid == USB_PID);
        let label = if is_dso { format!("{} (DSO-510 via COM name)", p.port_name) } else { p.port_name.clone() };
        v.push(PortChoice { label, path: p.port_name, is_dso: false });
    }
    v
}

/// The first connected DSO-510, if any.
pub fn find_dso() -> Option<PortChoice> {
    list().into_iter().find(|p| p.is_dso)
}

/// (interface path, serial number) of every present DSO-510.
#[cfg(windows)]
fn dso_interfaces() -> Vec<(String, String)> {
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::*;
    use windows_sys::core::GUID;

    const GUID_DEVINTERFACE_COMPORT: GUID = GUID::from_u128(0x86e0d1e0_8089_11d0_9ce4_08003e301f73);
    let needle = format!("vid_{USB_VID:04x}&pid_{USB_PID:04x}");
    let mut out = Vec::new();
    unsafe {
        let set = SetupDiGetClassDevsW(
            &GUID_DEVINTERFACE_COMPORT,
            std::ptr::null(),
            std::ptr::null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        );
        if set as isize == -1 {
            return out;
        }
        let mut index = 0;
        loop {
            let mut ifd: SP_DEVICE_INTERFACE_DATA = std::mem::zeroed();
            ifd.cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
            if SetupDiEnumDeviceInterfaces(set, std::ptr::null(), &GUID_DEVINTERFACE_COMPORT, index, &mut ifd) == 0 {
                break;
            }
            index += 1;
            // detail struct = u32 cbSize + UTF-16 path; use an aligned u32 buffer
            let mut buf = vec![0u32; 512];
            let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            (*detail).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            let mut needed = 0u32;
            if SetupDiGetDeviceInterfaceDetailW(set, &ifd, detail, (buf.len() * 4) as u32, &mut needed, std::ptr::null_mut()) == 0 {
                continue;
            }
            let p = std::ptr::addr_of!((*detail).DevicePath) as *const u16;
            let mut len = 0;
            while *p.add(len) != 0 && len < 1000 {
                len += 1;
            }
            let path = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
            if path.to_ascii_lowercase().contains(&needle) {
                // ...#VID_2E3C&PID_5740#<serial>#{guid}
                let serial = path.split('#').nth(2).unwrap_or("?").to_ascii_uppercase();
                out.push((path, serial));
            }
        }
        SetupDiDestroyDeviceInfoList(set);
    }
    out
}

#[cfg(not(windows))]
fn dso_interfaces() -> Vec<(String, String)> {
    Vec::new()
}
