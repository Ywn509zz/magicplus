//! Windows : RFCOMM par Winsock (`AF_BTH`). Les ordinateurs s'appairent dans
//! Paramètres → Bluetooth et appareils ; le service MagicPlus est publié dans l'annuaire SDP.

use super::{PairedDevice, SERVICE_UUID};
use crate::link::{BoxLink, Link};
use anyhow::{anyhow, bail, Result};
use std::{
    io::{self, Read, Write},
    mem::{size_of, zeroed},
    sync::Once,
    thread,
    time::Duration,
};
use windows_sys::{
    core::GUID,
    Win32::{
        Devices::Bluetooth::{
            BluetoothFindDeviceClose, BluetoothFindFirstDevice, BluetoothFindFirstRadio, BluetoothFindNextDevice,
            BluetoothFindRadioClose, AF_BTH, BLUETOOTH_DEVICE_INFO, BLUETOOTH_DEVICE_SEARCH_PARAMS,
            BLUETOOTH_FIND_RADIO_PARAMS, BTHPROTO_RFCOMM, NS_BTH, SOCKADDR_BTH,
        },
        Foundation::{CloseHandle, HANDLE},
        Networking::WinSock::{
            accept, bind, closesocket, connect as ws_connect, getsockname, listen as ws_listen, recv, send, setsockopt,
            shutdown, socket, WSAGetLastError, WSASetServiceW, WSAStartup, CSADDR_INFO, INVALID_SOCKET,
            RNRSERVICE_REGISTER, SD_BOTH, SOCKADDR, SOCKET, SOCK_STREAM, SOL_SOCKET, SO_RCVTIMEO, SO_SNDTIMEO, WSADATA,
            WSAQUERYSETW,
        },
    },
};

pub(super) const SUPPORTED: bool = true;
/// « Port » quelconque : Windows choisit un canal RFCOMM libre.
const BT_PORT_ANY: u32 = u32::MAX;

fn startup() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY : initialisation de Winsock (comptée, sans effet si déjà faite).
        unsafe {
            let mut data: WSADATA = zeroed();
            WSAStartup(0x0202, &mut data);
        }
    });
}

fn last_error(what: &str) -> anyhow::Error {
    // SAFETY : lecture de l'erreur Winsock du fil courant.
    let code = unsafe { WSAGetLastError() };
    anyhow!("{what} : {}", io::Error::from_raw_os_error(code))
}

fn format_address(address: u64) -> String {
    (0..6).rev().map(|i| format!("{:02x}", (address >> (i * 8)) & 0xff)).collect::<Vec<_>>().join("-")
}

fn parse_address(address: &str) -> Result<u64> {
    let hex: String = address.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 12 {
        bail!("adresse Bluetooth invalide : {address}");
    }
    Ok(u64::from_str_radix(&hex, 16)?)
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Prise RFCOMM ouverte.
struct Socket(SOCKET);

// SAFETY : une prise Winsock peut être utilisée depuis n'importe quel fil.
unsafe impl Send for Socket {}

impl Drop for Socket {
    fn drop(&mut self) {
        // SAFETY : la prise nous appartient.
        unsafe { closesocket(self.0) };
    }
}

impl Socket {
    fn set_timeout(&self, option: i32, timeout: Option<Duration>) -> io::Result<()> {
        let millis: u32 = timeout.map(|t| t.as_millis().clamp(1, u32::MAX as u128) as u32).unwrap_or(0);
        // SAFETY : option de la prise, valeur de 4 octets.
        let result = unsafe { setsockopt(self.0, SOL_SOCKET, option, &millis as *const u32 as *const u8, 4) };
        if result != 0 {
            // SAFETY : lecture de l'erreur Winsock.
            return Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }));
        }
        Ok(())
    }
}

impl Read for Socket {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let length = buf.len().min(i32::MAX as usize) as i32;
        // SAFETY : `buf` est valable pour `length` octets.
        let read = unsafe { recv(self.0, buf.as_mut_ptr(), length, 0) };
        if read < 0 {
            // SAFETY : lecture de l'erreur Winsock.
            return Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }));
        }
        Ok(read as usize)
    }
}

impl Write for Socket {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let length = buf.len().min(i32::MAX as usize) as i32;
        // SAFETY : `buf` est valable pour `length` octets.
        let sent = unsafe { send(self.0, buf.as_ptr(), length, 0) };
        if sent < 0 {
            // SAFETY : lecture de l'erreur Winsock.
            return Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }));
        }
        Ok(sent as usize)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Link for Socket {
    fn half_close(&self) -> bool {
        false
    }

    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.set_timeout(SO_RCVTIMEO, timeout)
    }

    fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.set_timeout(SO_SNDTIMEO, timeout)
    }

    fn close(&self) {
        // SAFETY : la prise nous appartient.
        unsafe { shutdown(self.0, SD_BOTH) };
    }
}

pub(super) fn powered() -> bool {
    // SAFETY : recherche de la radio Bluetooth (absente si le Bluetooth est coupé).
    unsafe {
        let params = BLUETOOTH_FIND_RADIO_PARAMS { dwSize: size_of::<BLUETOOTH_FIND_RADIO_PARAMS>() as u32 };
        let mut radio: HANDLE = std::ptr::null_mut();
        let find = BluetoothFindFirstRadio(&params, &mut radio);
        if find.is_null() {
            return false;
        }
        CloseHandle(radio);
        BluetoothFindRadioClose(find);
        true
    }
}

fn rfcomm_socket() -> Result<SOCKET> {
    startup();
    // SAFETY : création d'une prise RFCOMM.
    let handle = unsafe { socket(AF_BTH as i32, SOCK_STREAM, BTHPROTO_RFCOMM as i32) };
    if handle == INVALID_SOCKET {
        return Err(last_error("prise Bluetooth"));
    }
    Ok(handle)
}

pub(super) fn listen(on_link: Box<dyn Fn(BoxLink, String) + Send + Sync>) -> Result<()> {
    let server = Socket(rfcomm_socket()?);
    // SAFETY : structures C remplies ici, vivantes pendant les appels.
    unsafe {
        let mut address: SOCKADDR_BTH = zeroed();
        address.addressFamily = AF_BTH;
        address.port = BT_PORT_ANY;
        let size = size_of::<SOCKADDR_BTH>() as i32;
        if bind(server.0, &address as *const SOCKADDR_BTH as *const SOCKADDR, size) != 0 {
            return Err(last_error("canal Bluetooth"));
        }
        if ws_listen(server.0, 4) != 0 {
            return Err(last_error("écoute Bluetooth"));
        }
        let mut length = size;
        if getsockname(server.0, &mut address as *mut SOCKADDR_BTH as *mut SOCKADDR, &mut length) != 0 {
            return Err(last_error("canal Bluetooth"));
        }
        // Publication du service dans l'annuaire SDP : les autres le trouvent par son UUID.
        let mut service = GUID::from_u128(SERVICE_UUID);
        let mut name = wide("MagicPlus");
        let mut info: CSADDR_INFO = zeroed();
        info.LocalAddr.lpSockaddr = &mut address as *mut SOCKADDR_BTH as *mut SOCKADDR;
        info.LocalAddr.iSockaddrLength = size;
        info.iSocketType = SOCK_STREAM;
        info.iProtocol = BTHPROTO_RFCOMM as i32;
        let mut query: WSAQUERYSETW = zeroed();
        query.dwSize = size_of::<WSAQUERYSETW>() as u32;
        query.lpszServiceInstanceName = name.as_mut_ptr();
        query.lpServiceClassId = &mut service;
        query.dwNameSpace = NS_BTH;
        query.dwNumberOfCsAddrs = 1;
        query.lpcsaBuffer = &mut info;
        if WSASetServiceW(&query, RNRSERVICE_REGISTER, 0) != 0 {
            return Err(last_error("publication du service Bluetooth"));
        }
    }
    thread::spawn(move || loop {
        // SAFETY : attente d'une connexion sur notre prise d'écoute.
        let (client, remote) = unsafe {
            let mut remote: SOCKADDR_BTH = zeroed();
            let mut length = size_of::<SOCKADDR_BTH>() as i32;
            let client = accept(server.0, &mut remote as *mut SOCKADDR_BTH as *mut SOCKADDR, &mut length);
            (client, remote.btAddr)
        };
        if client == INVALID_SOCKET {
            thread::sleep(Duration::from_secs(1));
            continue;
        }
        on_link(Box::new(Socket(client)), format_address(remote));
    });
    Ok(())
}

pub(super) fn connect(address: &str) -> Result<BoxLink> {
    let target = parse_address(address)?;
    let client = Socket(rfcomm_socket()?);
    // SAFETY : adresse remplie ici ; Windows cherche le canal du service par son UUID (SDP).
    unsafe {
        let mut remote: SOCKADDR_BTH = zeroed();
        remote.addressFamily = AF_BTH;
        remote.btAddr = target;
        remote.serviceClassId = GUID::from_u128(SERVICE_UUID);
        remote.port = 0;
        if ws_connect(client.0, &remote as *const SOCKADDR_BTH as *const SOCKADDR, size_of::<SOCKADDR_BTH>() as i32) != 0 {
            return Err(last_error("connexion Bluetooth"));
        }
    }
    Ok(Box::new(client))
}

/// Ordinateurs appairés (classe « ordinateur ») connus de Windows.
pub(super) fn paired_computers() -> Vec<PairedDevice> {
    let mut devices = Vec::new();
    // SAFETY : énumération des appareils mémorisés, sans recherche radio.
    unsafe {
        let mut params: BLUETOOTH_DEVICE_SEARCH_PARAMS = zeroed();
        params.dwSize = size_of::<BLUETOOTH_DEVICE_SEARCH_PARAMS>() as u32;
        params.fReturnAuthenticated = 1;
        params.fReturnRemembered = 1;
        params.fReturnConnected = 1;
        let mut info: BLUETOOTH_DEVICE_INFO = zeroed();
        info.dwSize = size_of::<BLUETOOTH_DEVICE_INFO>() as u32;
        let find = BluetoothFindFirstDevice(&params, &mut info);
        if find.is_null() {
            return devices;
        }
        loop {
            let major = (info.ulClassofDevice >> 8) & 0x1F;
            if major == 1 && info.fAuthenticated != 0 {
                let name_length = info.szName.iter().position(|c| *c == 0).unwrap_or(info.szName.len());
                devices.push(PairedDevice {
                    address: format_address(info.Address.Anonymous.ullLong),
                    name: String::from_utf16_lossy(&info.szName[..name_length]),
                });
            }
            info = zeroed();
            info.dwSize = size_of::<BLUETOOTH_DEVICE_INFO>() as u32;
            if BluetoothFindNextDevice(find, &mut info) == 0 {
                break;
            }
        }
        BluetoothFindDeviceClose(find);
    }
    devices
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_round_trip() {
        assert_eq!(format_address(0x0011_22aa_bbcc), "00-11-22-aa-bb-cc");
        assert_eq!(parse_address("00-11-22-AA-BB-CC").unwrap(), 0x0011_22aa_bbcc);
        assert!(parse_address("00-11").is_err());
    }
}
