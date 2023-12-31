use crate::DanteDeviceEncoding::{PCM16, PCM24, PCM32};
use ascii::AsciiStr;
use bytes::BytesMut;
use log::{debug, error, info, warn};
use mdns_sd::{ServiceDaemon, ServiceEvent};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt::{Debug, Display, Formatter};
use std::hash::{Hash, Hasher};
use std::net::{Ipv4Addr, UdpSocket};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::Duration;

const CMC_SERVICE: &'static str = "_netaudio-cmc._udp.local.";
const DBC_SERVICE: &'static str = "_netaudio-dbc._udp.local.";
const ARC_SERVICE: &'static str = "_netaudio-arc._udp.local.";
const CHAN_SERVICE: &'static str = "_netaudio-chan._udp.local.";

const DEVICE_CONTROL_PORT: u32 = 8800;
const DEVICE_HEARTBEAT_PORT: u32 = 8708;
const DEVICE_INFO_PORT: u32 = 8702;
const DEVICE_INFO_SRC_PORT1: u32 = 1029;
const DEVICE_INFO_SRC_PORT2: u32 = 1030;

const DEVICE_SETTINGS_PORT: u32 = 8700;

#[derive(Clone)]
enum DanteDeviceEncoding {
    PCM16,
    PCM24,
    PCM32,
}

#[derive(Clone)]
struct DBCInfo {
    addresses: HashSet<Ipv4Addr>,
    port: u16,
}

#[derive(Clone)]
struct CMCInfo {
    addresses: HashSet<Ipv4Addr>,
    port: u16,
    id: String,
    manufacturer: String,
    model: String,
}

#[derive(Clone)]
struct ARCInfo {
    addresses: HashSet<Ipv4Addr>,
    port: u16,
    router_vers: String,
    router_info: String,
}

#[derive(Clone)]
struct CHANInfo {
    name: String,
    id: u16,
    sample_rate: u32,
    encoding: DanteDeviceEncoding,
    latency: Duration,
}

impl PartialEq<Self> for CHANInfo {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for CHANInfo {}

impl Hash for CHANInfo {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

#[derive(Debug)]
struct DeviceAlreadyPresent {}

impl Display for DeviceAlreadyPresent {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "Device not present.")
    }
}

impl std::error::Error for DeviceAlreadyPresent {}

#[derive(Debug)]
struct DeviceNotPresent {}

impl Display for DeviceNotPresent {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "Device not present.")
    }
}

impl std::error::Error for DeviceNotPresent {}

#[derive(Debug)]
struct DeviceStatus {
    connected_dbc: bool,
    connected_cmc: bool,
    connected_arc: bool,
    connected_chan: bool,
}

impl DeviceStatus {
    fn new() -> Self {
        DeviceStatus {
            connected_dbc: false,
            connected_cmc: false,
            connected_arc: false,
            connected_chan: false,
        }
    }
}

struct DeviceDiscoveryCache {
    dbc_info: Option<DBCInfo>,
    cmc_info: Option<CMCInfo>,
    arc_info: Option<ARCInfo>,
    chan_info: HashSet<CHANInfo>,
}

struct DanteDeviceList {
    devices: HashMap<String, DeviceStatus>,
    caches: HashMap<String, DeviceDiscoveryCache>,
}

impl DanteDeviceList {
    /// Adds a new device to the list. Will return error when the device is already in the list.
    fn add_device(&mut self, new_device_name: &str) -> Result<(), DeviceAlreadyPresent> {
        if self.devices.contains_key(new_device_name) {
            return Err(DeviceAlreadyPresent {});
        }

        self.devices
            .insert(new_device_name.to_owned(), DeviceStatus::new());

        // Create a cache for the device as well if there isn't already one.
        if !self.caches.contains_key(new_device_name) {
            self.caches.insert(
                new_device_name.to_owned(),
                DeviceDiscoveryCache {
                    dbc_info: None,
                    cmc_info: None,
                    arc_info: None,
                    chan_info: HashSet::new(),
                },
            );
        }

        Ok(())
    }

    fn try_add_device(&mut self, new_device_name: &str) {
        // Explicitly throw away error. If we already had one, Ok. If we make one, also Ok.
        let _ = self.add_device(new_device_name);
    }

    fn device_connected(&self, device_name: &str) -> bool {
        self.devices.contains_key(device_name)
    }

    fn channel_id_exist(&self, device_name: &str, chan_id: u16) -> bool {
        if !(self.device_connected(device_name)) {
            return false;
        }
        match self.caches.get(device_name) {
            Some(cache) => cache
                .chan_info
                .iter()
                .any(|chan_info| chan_info.id == chan_id),
            None => {
                error!("Cache doesn't exist despite device being connected!");
                false
            }
        }
    }

    fn get_channel_name_from_id(&self, device_name: &str, chan_id: u16) -> Option<&str> {
        if !(self.device_connected(device_name)) {
            return None;
        }

        match self.caches.get(device_name) {
            Some(cache) => match cache
                .chan_info
                .iter()
                .find(|chan_info| chan_info.id == chan_id)
            {
                Some(chan) => Some(&chan.name),
                None => None,
            },
            None => {
                error!("Cache doesn't exist despite device being connected!");
                None
            }
        }
    }

    fn get_device_arc_ips(&self, device_name: &str) -> Option<&HashSet<Ipv4Addr>> {
        if !(self.device_connected(device_name)) {
            return None;
        }

        Some(&self.caches.get(device_name)?.arc_info.as_ref()?.addresses)
    }

    /// Updates the dbc info of device in the list with a specific name. If it doesn't exist, will add it then update it.
    fn update_dbc(&mut self, device_name: &str, info: DBCInfo) {
        self.caches
            .get_mut(device_name)
            .expect("Tried updating cache of device that doesn't exist")
            .dbc_info = Some(info);
        debug!("update_dbc for {}", device_name);
    }

    /// Updates the cmc info of device in the list with a specific name. If it doesn't exist, will add it then update it.
    fn update_cmc(&mut self, device_name: &str, info: CMCInfo) {
        self.caches
            .get_mut(device_name)
            .expect("Tried updating cache of device that doesn't exist")
            .cmc_info = Some(info);
        debug!("update_cmc for {}", device_name);
    }

    /// Updates the arc info of device in the list with a specific name. If it doesn't exist, will add it then update it.
    fn update_arc(&mut self, device_name: &str, info: ARCInfo) {
        self.caches
            .get_mut(device_name)
            .expect("Tried updating cache of device that doesn't exist")
            .arc_info = Some(info);
        debug!("update_arc for {}", device_name);
    }

    /// Updates the cmc info of device in the list with a specific name. If it doesn't exist, will add it then update it.
    fn update_chan(&mut self, device_name: &str, info: CHANInfo) {
        self.caches
            .get_mut(device_name)
            .expect("Tried updating cache of device that doesn't exist")
            .chan_info
            .replace(info);
        debug!("update_chan for {}", device_name);
    }

    fn connect_dbc(&mut self, device_name: &str) {
        self.try_add_device(device_name);
        self.devices
            .get_mut(device_name)
            .expect("Just tried to add device, should be able to get it")
            .connected_dbc = true;
        debug!("Connected to dbc discovery.");
    }

    fn connect_cmc(&mut self, device_name: &str) {
        self.try_add_device(device_name);
        self.devices
            .get_mut(device_name)
            .expect("Just tried to add device, should be able to get it")
            .connected_cmc = true;
        debug!("Connected to cmc discovery.");
    }

    fn connect_arc(&mut self, device_name: &str) {
        self.try_add_device(device_name);
        self.devices
            .get_mut(device_name)
            .expect("Just tried to add device, should be able to get it")
            .connected_arc = true;
        debug!("Connected to arc discovery.");
    }

    fn connect_chan(&mut self, device_name: &str) {
        self.try_add_device(device_name);
        self.devices
            .get_mut(device_name)
            .expect("Just tried to add device, should be able to get it")
            .connected_chan = true;
        debug!("Connected to chan discovery.");
    }

    fn disconnect_dbc(&mut self, device_name: &str) {
        self.devices
            .get_mut(device_name)
            .expect("If we're calling disconnect, we should still have the device in the list.")
            .connected_dbc = false;
        self.check_remove(device_name)
            .expect("If we're calling disconnect, we should still have the device in the list.");
        debug!("Disconnected from dbc discovery");
    }

    fn disconnect_cmc(&mut self, device_name: &str) {
        self.devices
            .get_mut(device_name)
            .expect("If we're calling disconnect, we should still have the device in the list.")
            .connected_cmc = false;
        self.check_remove(device_name)
            .expect("If we're calling disconnect, we should still have the device in the list.");
        debug!("Disconnected from cmc discovery");
    }

    fn disconnect_arc(&mut self, device_name: &str) {
        self.devices
            .get_mut(device_name)
            .expect("If we're calling disconnect, we should still have the device in the list.")
            .connected_arc = false;
        self.check_remove(device_name)
            .expect("If we're calling disconnect, we should still have the device in the list.");
        debug!("Disconnected from arc discovery");
    }

    fn disconnect_chan(&mut self, device_name: &str) {
        self.devices
            .get_mut(device_name)
            .expect("If we're calling disconnect, we should still have the device in the list.")
            .connected_chan = false;
        self.check_remove(device_name)
            .expect("If we're calling disconnect, we should still have the device in the list.");
        debug!("Disconnected from chan discovery");
    }

    /// Checks if a device should be removed (all the discovery types have been removed), and deletes if if that's the case.
    /// Errors when the device name isn't a device in the list.
    fn check_remove(&mut self, device_name: &str) -> Result<(), DeviceNotPresent> {
        match self.devices.get(device_name) {
            Some(device_status) => {
                if !(device_status.connected_dbc
                    || device_status.connected_cmc
                    || device_status.connected_arc
                    || device_status.connected_chan)
                {
                    self.devices.remove(device_name);
                }

                Ok(())
            }
            None => Err(DeviceNotPresent {}),
        }
    }

    fn new() -> Self {
        DanteDeviceList {
            devices: HashMap::new(),
            caches: HashMap::new(),
        }
    }
}

/// Cutoff the address from a hostname. Address default is "local."
fn cutoff_address<'a>(hostname: &'a str, address: Option<&'a str>) -> &'a str {
    let cutoff_string = ".".to_string() + address.unwrap_or("local.");
    match hostname.strip_suffix(&cutoff_string) {
        None => {
            warn!(
                "Device \"{}\" doesn't end with \"{}\". This is abnormal.",
                hostname, cutoff_string
            );
            hostname
        }
        Some(stripped) => stripped,
    }
}

#[derive(thiserror::Error, Debug)]
pub enum MakeSubscriptionError {
    #[error("transmitter device not connected")]
    TXDeviceNotConnected,
    #[error("receiver device not connected")]
    RXDeviceNotConnected,
    #[error("transmitter channel doesn't exist")]
    TXChannelNotExist,
    #[error("receiver channel doesn't exist")]
    RXChannelNotExist,
    #[error("the device name + channel name byte length is greater than 107")]
    TXChannelPlusDeviceNameLengthInvalid,
    #[error("error sending udp packet")]
    ConnectionFailed,
    #[error("no arc ips have been captured")]
    NoArcIPs,
}

pub struct DanteDeviceManager {
    device_list: Arc<Mutex<DanteDeviceList>>,
    running: Arc<Mutex<bool>>,
    current_command_sequence_id: u16,
}

impl DanteDeviceManager {
    /// Spawns the discovery service in a separate thread. Call stop_discovery() to end it.
    pub fn start_discovery(&self) -> Result<(), Box<dyn std::error::Error>> {
        info!("Starting discovery");
        *self.running.lock().unwrap() = true;

        // Spawn threads equal to the number of different addresses we are discovering on.
        let mdns = ServiceDaemon::new().expect("Failed to create mdns service daemon!");

        // Discovery for DBC
        let dbc_receiver = mdns
            .browse(DBC_SERVICE)
            .unwrap_or_else(|_| panic!("Failed to browse for {}", DBC_SERVICE));

        // Fresh Arcs to move into thread.
        let device_list_dbc = self.device_list.clone();
        let running_dbc = self.running.clone();

        let dbc_thread = std::thread::spawn(move || {
            debug!("Starting discovery thread");
            while *running_dbc.lock().unwrap() {
                while let Ok(event) = dbc_receiver.try_recv() {
                    match event {
                        ServiceEvent::SearchStarted(service_type) => {
                            debug!("DBC Search Started: {}", &service_type);
                        }
                        ServiceEvent::ServiceFound(service_type, fullname) => {
                            debug!("DBC Search Found: {}, {}", &service_type, &fullname);
                            let device_name = cutoff_address(&fullname, Some(DBC_SERVICE));

                            let mut device_list_lock = device_list_dbc
                                .lock()
                                .expect("Cannot get mutex lock of DanteDevices");

                            device_list_lock.connect_dbc(device_name);
                        }
                        ServiceEvent::ServiceResolved(service_info) => {
                            info!("DBC Service Resolved: {:?}", &service_info);
                            let device_name =
                                cutoff_address(service_info.get_fullname(), Some(DBC_SERVICE));
                            let mut device_list_lock = device_list_dbc
                                .lock()
                                .expect("Cannot get mutex lock of DanteDevices");
                            device_list_lock.update_dbc(
                                device_name,
                                DBCInfo {
                                    addresses: service_info.get_addresses().to_owned(),
                                    port: service_info.get_port().to_owned(),
                                },
                            );
                        }
                        ServiceEvent::ServiceRemoved(service_type, fullname) => {
                            info!("DBC Service Removed: a:{}, b:{}", &service_type, &fullname);
                            let mut device_list_lock = device_list_dbc.lock().unwrap();
                            device_list_lock
                                .disconnect_dbc(cutoff_address(&fullname, Some(DBC_SERVICE)));
                        }
                        ServiceEvent::SearchStopped(service_type) => {
                            error!("DBC Search Stopped: {}", &service_type);
                        }
                    }
                }
                sleep(Duration::from_millis(100));
            }
        });

        // Discovery for CMC
        let cmc_receiver = mdns
            .browse(CMC_SERVICE)
            .unwrap_or_else(|_| panic!("Failed to browse for {}", CMC_SERVICE));

        // Fresh Arcs to move into thread.
        let device_list_cmc = self.device_list.clone();
        let running_cmc = self.running.clone();

        let cmc_thread = std::thread::spawn(move || {
            debug!("Starting discovery thread");
            while *running_cmc.lock().unwrap() {
                while let Ok(event) = cmc_receiver.try_recv() {
                    match event {
                        ServiceEvent::SearchStarted(service_type) => {
                            debug!("CMC Search Started: {}", &service_type);
                        }
                        ServiceEvent::ServiceFound(service_type, fullname) => {
                            debug!("CMC Search Found: {}, {}", &service_type, &fullname);
                            let device_name = cutoff_address(&fullname, Some(CMC_SERVICE));

                            let mut device_list_lock = device_list_cmc
                                .lock()
                                .expect("Cannot get mutex lock of DanteDevices");

                            device_list_lock.connect_cmc(device_name);
                        }
                        ServiceEvent::ServiceResolved(service_info) => {
                            info!("CMC Service Resolved: {:?}", &service_info);
                            let device_name =
                                cutoff_address(service_info.get_fullname(), Some(CMC_SERVICE));
                            let mut device_list_lock = device_list_cmc
                                .lock()
                                .expect("Cannot get mutex lock of DanteDevices");
                            device_list_lock.update_cmc(
                                device_name,
                                CMCInfo {
                                    addresses: service_info.get_addresses().to_owned(),
                                    port: service_info.get_port().to_owned(),
                                    id: service_info
                                        .get_property("id")
                                        .expect(
                                            "Could not retrieve \"id\" property from cmc service",
                                        )
                                        .val_str()
                                        .to_owned(),
                                    manufacturer: match service_info.get_property("mf") {
                                        Some(mf_property) => mf_property.val_str().to_owned(),
                                        None => "N/A".to_string(),
                                    },
                                    model: match service_info.get_property("model") {
                                        Some(model_property) => model_property.val_str().to_owned(),
                                        None => "N/A".to_string(),
                                    },
                                },
                            );
                        }
                        ServiceEvent::ServiceRemoved(service_type, fullname) => {
                            info!("CMC Service Removed: a:{}, b:{}", &service_type, &fullname);
                            let mut device_list_lock = device_list_cmc.lock().unwrap();
                            device_list_lock
                                .disconnect_cmc(cutoff_address(&fullname, Some(CMC_SERVICE)));
                        }
                        ServiceEvent::SearchStopped(service_type) => {
                            error!("CMC Search Stopped: {}", &service_type);
                        }
                    }
                }
                sleep(Duration::from_millis(100));
            }
        });

        // Discovery for ARC
        let arc_receiver = mdns
            .browse(ARC_SERVICE)
            .unwrap_or_else(|_| panic!("Failed to browse for {}", ARC_SERVICE));

        // Fresh Arcs to move into thread.
        let device_list_arc = self.device_list.clone();
        let running_arc = self.running.clone();

        let arc_thread = std::thread::spawn(move || {
            debug!("Starting discovery thread");
            while *running_arc.lock().unwrap() {
                while let Ok(event) = arc_receiver.try_recv() {
                    match event {
                        ServiceEvent::SearchStarted(service_type) => {
                            debug!("ARC Search Started: {}", &service_type);
                        }
                        ServiceEvent::ServiceFound(service_type, fullname) => {
                            debug!("ARC Search Found: {}, {}", &service_type, &fullname);
                            let device_name = cutoff_address(&fullname, Some(ARC_SERVICE));

                            let mut device_list_lock = device_list_arc
                                .lock()
                                .expect("Cannot get mutex lock of DanteDevices");

                            device_list_lock.connect_arc(device_name);
                        }
                        ServiceEvent::ServiceResolved(service_info) => {
                            info!("ARC Service Resolved: {:?}", &service_info);
                            let device_name =
                                cutoff_address(service_info.get_fullname(), Some(ARC_SERVICE));
                            let mut device_list_lock = device_list_arc
                                .lock()
                                .expect("Cannot get mutex lock of DanteDevices");
                            device_list_lock.update_arc(
                                device_name,
                                ARCInfo {
                                    addresses: service_info.get_addresses().to_owned(),
                                    port: service_info.get_port().to_owned(),
                                    router_vers: match service_info.get_property("router_vers") {
                                        Some(router_vers_property) => {
                                            router_vers_property.val_str().to_owned()
                                        }
                                        None => "N/A".to_string(),
                                    },
                                    router_info: match service_info.get_property("router_info") {
                                        Some(router_info_property) => {
                                            router_info_property.val_str().to_owned()
                                        }
                                        None => "N/A".to_string(),
                                    },
                                },
                            );
                        }
                        ServiceEvent::ServiceRemoved(service_type, fullname) => {
                            info!("ARC Service Removed: a:{}, b:{}", &service_type, &fullname);
                            let mut device_list_lock = device_list_arc.lock().unwrap();
                            device_list_lock
                                .disconnect_arc(cutoff_address(&fullname, Some(ARC_SERVICE)));
                        }
                        ServiceEvent::SearchStopped(service_type) => {
                            error!("ARC Search Stopped: {}", &service_type);
                        }
                    }
                }
                sleep(Duration::from_millis(100));
            }
        });

        // Discovery for CHAN
        let chan_receiver = mdns
            .browse(CHAN_SERVICE)
            .unwrap_or_else(|_| panic!("Failed to browse for {}", CHAN_SERVICE));

        // Fresh Arcs to move into thread.
        let device_list_chan = self.device_list.clone();
        let running_chan = self.running.clone();

        let chan_thread = std::thread::spawn(move || {
            debug!("Starting discovery thread");
            while *running_chan.lock().unwrap() {
                while let Ok(event) = chan_receiver.try_recv() {
                    match event {
                        ServiceEvent::SearchStarted(service_type) => {
                            debug!("CHAN Search Started: {}", &service_type);
                        }
                        ServiceEvent::ServiceFound(service_type, fullname) => {
                            debug!("CHAN Search Found: {}, {}", &service_type, &fullname);
                            let (chan_name, full_name) = fullname
                                .split_once("@")
                                .expect("CHAN fullname without \"@\" unexpected.");
                            let device_name = cutoff_address(full_name, Some(CHAN_SERVICE));

                            let mut device_list_lock = device_list_chan
                                .lock()
                                .expect("Cannot get mutex lock of DanteDevices");

                            device_list_lock.connect_chan(device_name);
                        }
                        ServiceEvent::ServiceResolved(service_info) => {
                            info!("CHAN Service Resolved: {:?}", &service_info);
                            let (chan_name, full_name) = service_info
                                .get_fullname()
                                .split_once("@")
                                .expect("CHAN fullname without \"@\" unexpected.");
                            let device_name = cutoff_address(full_name, Some(CHAN_SERVICE));
                            let mut device_list_lock = device_list_chan
                                .lock()
                                .expect("Cannot get mutex lock of DanteDevices");
                            device_list_lock.update_chan(
                                device_name,
                                CHANInfo {
                                    name: chan_name.to_owned(),
                                    id: service_info
                                        .get_property("id")
                                        .expect("Should be able to get id property")
                                        .val_str()
                                        .parse()
                                        .expect("Couldn't parse ID"),
                                    sample_rate: service_info
                                        .get_property("rate")
                                        .expect("Should be able to get rate property")
                                        .val_str()
                                        .parse()
                                        .expect("Couldn't parse rate"),
                                    encoding: match service_info
                                        .get_property("en")
                                        .expect("Should be able to get encoding property")
                                        .val_str()
                                    {
                                        "16" => PCM16,
                                        "24" => PCM24,
                                        "32" => PCM32,
                                        &_ => {
                                            panic!("\"en\" property couldn't be parsed into a valid encoding. IE: 16, 24, or 32");
                                        }
                                    },
                                    latency: Duration::from_nanos(service_info.get_property("latency_ns").expect("Should be able to get latency_ns property").val_str().parse().expect("Couldn't parse latency_ns")),
                                },
                            );
                        }
                        ServiceEvent::ServiceRemoved(service_type, fullname) => {
                            info!("CHAN Service Removed: a:{}, b:{}", &service_type, &fullname);
                            let (chan_name, full_name) = fullname
                                .split_once("@")
                                .expect("CHAN fullname without \"@\" unexpected.");
                            let device_name = cutoff_address(full_name, Some(CHAN_SERVICE));

                            let mut device_list_lock = device_list_chan.lock().unwrap();
                            device_list_lock.disconnect_chan(device_name);
                        }
                        ServiceEvent::SearchStopped(service_type) => {
                            error!("CHAN Search Stopped: {}", &service_type);
                        }
                    }
                }
                sleep(Duration::from_millis(100));
            }
        });

        Ok(())
    }

    fn get_new_command_sequence_id(&mut self) -> u16 {
        let return_id = self.current_command_sequence_id;
        self.current_command_sequence_id += 1;
        return_id
    }

    const COMMAND_CHANNELCOUNT: [u8; 2] = 1000u16.to_be_bytes();
    const COMMAND_DEVICEINFO: [u8; 2] = 1003u16.to_be_bytes();
    const COMMAND_DEVICENAME: [u8; 2] = 1002u16.to_be_bytes();
    const COMMAND_SUBSCRIPTION: [u8; 2] = 3010u16.to_be_bytes();
    const COMMAND_RXCHANNELNAMES: [u8; 2] = 3000u16.to_be_bytes();
    const COMMAND_TXCHANNELNAMES: [u8; 2] = 2010u16.to_be_bytes();
    const COMMAND_SETRXCHANNELNAME: [u8; 2] = 12289u16.to_be_bytes();
    const COMMAND_SETTXCHANNELNAME: [u8; 2] = 8211u16.to_be_bytes();
    const COMMAND_SETDEVICENAME: [u8; 2] = 4097u16.to_be_bytes();

    fn make_dante_command(&mut self, command: [u8; 2], command_args: &[u8]) -> BytesMut {
        let mut buffer = bytes::BytesMut::new();
        buffer.extend_from_slice(&[0x27, 0x29]);
        assert_eq!(buffer.len(), 2);
        buffer.extend_from_slice(&((command_args.len() + 11) as u16).to_be_bytes());
        assert_eq!(buffer.len(), 4);
        buffer.extend_from_slice(&self.get_new_command_sequence_id().to_be_bytes());
        assert_eq!(buffer.len(), 6);
        buffer.extend(command);
        assert_eq!(buffer.len(), 8);
        buffer.extend_from_slice(&[0x00, 0x00]);
        assert_eq!(buffer.len(), 10);
        buffer.extend_from_slice(&command_args);
        buffer.extend_from_slice(&[0x00]);
        buffer
    }

    fn send_bytes_to_addresses(
        addresses: &HashSet<Ipv4Addr>,
        port: u16,
        bytes: &[u8],
    ) -> Result<(), Box<dyn Error>> {
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        for address in addresses {
            debug!("Sent bytes {:?} to {}:{}", bytes, address, port);
            socket.send_to(bytes, (*address, port))?;
        }
        Ok(())
    }

    pub fn make_subscription(
        &mut self,
        rx_device: &AsciiStr,
        rx_channel_id: u16,
        tx_device: &AsciiStr,
        tx_channel_id: u16,
    ) -> Result<(), MakeSubscriptionError> {
        let mut device_list_lock = self.device_list.lock().unwrap();

        if !device_list_lock.device_connected(rx_device.as_str()) {
            return Err(MakeSubscriptionError::RXDeviceNotConnected);
        }
        if !device_list_lock.device_connected(tx_device.as_str()) {
            return Err(MakeSubscriptionError::TXDeviceNotConnected);
        }
        if !device_list_lock.channel_id_exist(rx_device.as_str(), rx_channel_id) {
            return Err(MakeSubscriptionError::RXChannelNotExist);
        }
        if !device_list_lock.channel_id_exist(tx_device.as_str(), tx_channel_id) {
            return Err(MakeSubscriptionError::TXChannelNotExist);
        }

        let tx_device_name_buffer = tx_device.as_bytes();
        let tx_channel_name_buffer =
            match device_list_lock.get_channel_name_from_id(tx_device.as_str(), tx_channel_id) {
                Some(name) => name,
                None => {
                    return Err(MakeSubscriptionError::TXChannelNotExist);
                }
            }
            .as_bytes();

        let mut command_buffer = BytesMut::new();
        command_buffer.extend_from_slice(&[0x04, 0x01]);
        assert_eq!(command_buffer.len(), 2);
        command_buffer.extend_from_slice(&rx_channel_id.to_be_bytes());
        assert_eq!(command_buffer.len(), 4);
        command_buffer.extend_from_slice(&[0x00, 0x5c, 0x00, 0x6d]);
        assert_eq!(command_buffer.len(), 8);
        if (107 - tx_channel_name_buffer.len() as i32 - tx_device_name_buffer.len() as i32) < 0 {
            return Err(MakeSubscriptionError::TXChannelPlusDeviceNameLengthInvalid);
        }
        command_buffer.extend_from_slice(&vec![
            0x00;
            107 - tx_channel_name_buffer.len()
                - tx_device_name_buffer.len()
        ]);
        command_buffer.extend_from_slice(tx_channel_name_buffer);
        command_buffer.extend_from_slice(&[0x00]);
        command_buffer.extend_from_slice(tx_device_name_buffer);

        let addresses = match device_list_lock.get_device_arc_ips(rx_device.as_str()) {
            Some(addresses) => addresses.clone(),
            None => {
                return Err(MakeSubscriptionError::NoArcIPs);
            }
        };

        drop(device_list_lock);

        match Self::send_bytes_to_addresses(
            &addresses,
            0,
            &self.make_dante_command(Self::COMMAND_SUBSCRIPTION, &command_buffer),
        ) {
            Ok(_) => Ok(()),
            Err(_) => Err(MakeSubscriptionError::ConnectionFailed),
        }
    }

    pub fn is_running(&self) -> bool {
        *self.running.lock().unwrap()
    }

    pub fn stop_discovery(&self) {
        *self.running.lock().unwrap() = false;
    }

    pub fn get_device_names(&self) -> Vec<String> {
        self.device_list
            .lock()
            .unwrap()
            .devices
            .keys()
            .map(|device| device.to_owned())
            .collect()
    }

    pub fn get_device_descriptions(&self) -> Vec<String> {
        let device_list = self.device_list.lock().unwrap();
        let device_info_map = device_list.devices.iter().map(|(device, status)| {
            (
                device,
                status,
                device_list
                    .caches
                    .get(device)
                    .expect("Should have a cache for any given connected device."),
            )
        });
        device_info_map.into_iter()
            .map(|(device, status, cache)| {
                let mut info = format!(
                    "{}:\ndbc status: {}\ncmc status: {}\narc status: {}\nchan status: {}\nid: {}\nmanufacturer: {}\nmodel: {}\nrouter_vers: {}\nrouter_info: {}\nARC port: {}",
                    device,
                    match status.connected_dbc {
                        true => "Connected",
                        false => "Disconnected",
                    },
                    match status.connected_cmc {
                        true => "Connected",
                        false => "Disconnected",
                    },
                    match status.connected_arc {
                        true => "Connected",
                        false => "Disconnected",
                    },
                    match status.connected_chan {
                        true => "Connected",
                        false => "Disconnected",
                    },
                    match &cache.cmc_info {
                        Some(cmc_info) => {cmc_info.id.to_owned()}
                        None => "N/A".to_string()
                    },
                    match &cache.cmc_info {
                        Some(cmc_info) => {cmc_info.manufacturer.to_owned()}
                        None => "N/A".to_string()
                    },
                    match &cache.cmc_info {
                        Some(cmc_info) => {cmc_info.model.to_owned()}
                        None => "N/A".to_string()
                    },
                    match &cache.arc_info {
                        Some(arc_info) => {arc_info.router_vers.to_owned()}
                        None => "N/A".to_string()
                    },
                    match &cache.arc_info {
                        Some(arc_info) => {arc_info.router_info.to_owned()}
                        None => "N/A".to_string()
                    },
                    match &cache.arc_info {
                        Some(arc_info) => {arc_info.port.to_string()}
                        None => "N/A".to_string()
                    }
                );
                info += "\nChannels:";
                let mut chan_info_sorted: Vec<&CHANInfo> = cache.chan_info.iter().collect();
                chan_info_sorted.sort_by(|x, y| x.id.partial_cmp(&y.id).unwrap());
                for chan_info in chan_info_sorted {
                    info += &format!("\n\"{}\"", chan_info.name);
                }
                info
            })
            .collect()
    }

    pub fn new() -> Self {
        DanteDeviceManager {
            device_list: Arc::new(Mutex::new(DanteDeviceList::new())),
            running: Arc::new(Mutex::new(false)),
            current_command_sequence_id: 0,
        }
    }
}

impl Default for DanteDeviceManager {
    fn default() -> Self {
        DanteDeviceManager::new()
    }
}

/// Print raw data received from mDNS discovery requests at addr.
fn print_mdns_with_address(addr: &str, poll_time: Duration) {
    info!("Starting discovery");

    let mdns = ServiceDaemon::new().expect("Failed to create mdns service daemon!");
    let receiver = mdns
        .browse(addr)
        .unwrap_or_else(|_| panic!("Failed to browse for {}", addr));

    let keep_polling = Arc::new(Mutex::new(true));
    let keep_polling_thread = keep_polling.clone();

    let thread = std::thread::spawn(move || {
        debug!("Starting discovery thread");
        while *keep_polling_thread.lock().unwrap() {
            while let Ok(event) = receiver.try_recv() {
                match event {
                    ServiceEvent::SearchStarted(service_name) => {
                        println!("Search Started: {}", &service_name)
                    }
                    ServiceEvent::ServiceFound(service_name, host_service_name) => {
                        println!("Search Found: {}, {}", &service_name, &host_service_name)
                    }
                    ServiceEvent::ServiceResolved(service_info) => {
                        println!("Service Resolved: {:?}", &service_info);
                    }
                    ServiceEvent::ServiceRemoved(a, b) => {
                        println!("Service Removed: {}, {}", &a, &b)
                    }
                    ServiceEvent::SearchStopped(a) => {
                        println!("Search Stopped: {}", &a)
                    }
                }
            }
            sleep(Duration::from_millis(100));
        }
    });

    sleep(poll_time);

    *keep_polling.lock().unwrap() = false;

    thread.join().unwrap();
}

/// Print raw data received from mDNS discovery requests to the "_netaudio-cmc._udp.local." address.
pub fn print_cmc(poll_time: Duration) {
    print_mdns_with_address(CMC_SERVICE, poll_time);
}

/// Print raw data received from mDNS discovery requests to the "_netaudio-dbc._udp.local." address.
pub fn print_dbc(poll_time: Duration) {
    print_mdns_with_address(DBC_SERVICE, poll_time);
}

/// Print raw data received from mDNS discovery requests to the "_netaudio-arc._udp.local." address.
pub fn print_arc(poll_time: Duration) {
    print_mdns_with_address(ARC_SERVICE, poll_time);
}

/// Print raw data received from mDNS discovery requests to the "_netaudio-chan._udp.local." address.
pub fn print_chan(poll_time: Duration) {
    print_mdns_with_address(CHAN_SERVICE, poll_time);
}
