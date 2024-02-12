use crate::DanteDeviceEncoding::{PCM16, PCM24, PCM32};
use ascii::AsciiStr;
use bytes::BytesMut;
use log::{debug, error, info, warn};
use mdns_sd::{ServiceDaemon, ServiceEvent};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt::{Debug, Display, Formatter, Write};
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

#[derive(Debug)]
pub enum DanteVersion {
    Dante4_4_1_3,
    Dante4_2_1_3,
}

impl Display for DanteVersion {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!(
            "{}",
            match self {
                DanteVersion::Dante4_4_1_3 => "4.4.1.3",
                DanteVersion::Dante4_2_1_3 => "4.2.1.3",
            }
        ))
    }
}

impl DanteVersion {
    fn get_commands(&self) -> DanteVersionCommands {
        match self {
            DanteVersion::Dante4_4_1_3 => DANTECOMMANDS_4_4_1_3,
            DanteVersion::Dante4_2_1_3 => DANTECOMMANDS_4_2_1_3,
        }
    }

    pub fn from_string(string: &str) -> Option<Self> {
        match string {
            "4.4.1.3" => Some(Self::Dante4_4_1_3),
            "4.2.1.3" => Some(Self::Dante4_2_1_3),
            _ => None,
        }
    }
}

struct DanteVersionCommands {
    command_subscription: [u8; 2],
}

// Command IDs for different Dante Versions.
const DANTECOMMANDS_4_4_1_3: DanteVersionCommands = DanteVersionCommands {
    command_subscription: [0x34, 0x10],
};
const DANTECOMMANDS_4_2_1_3: DanteVersionCommands = DanteVersionCommands {
    command_subscription: [0x30, 0x10],
};

// Still need to figure these out.
/*
const COMMAND_CHANNELCOUNT: [u8; 2] = 1000u16.to_be_bytes();
const COMMAND_DEVICEINFO: [u8; 2] = 1003u16.to_be_bytes();
const COMMAND_DEVICENAME: [u8; 2] = 1002u16.to_be_bytes();
const COMMAND_RXCHANNELNAMES: [u8; 2] = 3000u16.to_be_bytes();
const COMMAND_TXCHANNELNAMES: [u8; 2] = 2010u16.to_be_bytes();
const COMMAND_SETRXCHANNELNAME: [u8; 2] = 12289u16.to_be_bytes();
const COMMAND_SETTXCHANNELNAME: [u8; 2] = 8211u16.to_be_bytes();
const COMMAND_SETDEVICENAME: [u8; 2] = 4097u16.to_be_bytes();
 */

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
    id: Option<u16>,
    sample_rate: Option<u32>,
    encoding: Option<DanteDeviceEncoding>,
    latency: Option<Duration>,
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
            Some(cache) => cache.chan_info.iter().any(|chan_info| match chan_info.id {
                Some(chan_info_id) => chan_info_id == chan_id,
                None => false,
            }),
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
            Some(cache) => match cache.chan_info.iter().find(|chan_info| match chan_info.id {
                Some(chan_info_id) => chan_info_id == chan_id,
                None => false,
            }) {
                Some(chan) => Some(&chan.name),
                None => None,
            },
            None => {
                error!("Cache doesn't exist despite device being connected!");
                None
            }
        }
    }

    fn get_device_ips(&self, device_name: &str) -> Option<HashSet<Ipv4Addr>> {
        if !(self.device_connected(device_name)) {
            return None;
        }

        let mut device_ips: HashSet<Ipv4Addr> = HashSet::new();

        match self.caches.get(device_name) {
            None => return None,
            Some(caches) => {
                if let Some(arc_info) = &caches.arc_info {
                    device_ips.extend(&arc_info.addresses);
                }
                if let Some(dbc_info) = &caches.dbc_info {
                    device_ips.extend(&dbc_info.addresses);
                }
                if let Some(cmc_info) = &caches.cmc_info {
                    device_ips.extend(&cmc_info.addresses);
                }
            }
        }

        Some(device_ips)
    }

    /// Updates the dbc info of device in the list with a specific name.
    fn update_dbc(&mut self, device_name: &str, info: DBCInfo) {
        self.caches
            .get_mut(device_name)
            .expect("Tried updating cache of device that doesn't exist")
            .dbc_info = Some(info);
        debug!("update_dbc for {}", device_name);
    }

    /// Updates the cmc info of device in the list with a specific name.
    fn update_cmc(&mut self, device_name: &str, info: CMCInfo) {
        self.caches
            .get_mut(device_name)
            .expect("Tried updating cache of device that doesn't exist")
            .cmc_info = Some(info);
        debug!("update_cmc for {}", device_name);
    }

    /// Updates the arc info of device in the list with a specific name.
    fn update_arc(&mut self, device_name: &str, info: ARCInfo) {
        self.caches
            .get_mut(device_name)
            .expect("Tried updating cache of device that doesn't exist")
            .arc_info = Some(info);
        debug!("update_arc for {}", device_name);
    }

    /// Updates the cmc info of device in the list with a specific name.
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
    #[error("error sending udp packet")]
    ConnectionFailed,
}
#[derive(thiserror::Error, Debug)]
pub enum ClearSubscriptionError {
    #[error("error sending udp packet")]
    ConnectionFailed,
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
                                    id: match service_info.get_property("id") {
                                        Some(id_property) => id_property.val_str().to_owned(),
                                        None => "N/A".to_string(),
                                    },
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
                                    id: service_info.get_property("id").map(|id_property| {
                                        id_property
                                            .val_str()
                                            .to_owned()
                                            .parse()
                                            .expect("Couldn't parse chan service id")
                                    }),
                                    sample_rate: match service_info.get_property("rate") {
                                        Some(rate_property) => rate_property.val_str().parse().ok(),
                                        None => None,
                                    },
                                    encoding: match service_info.get_property("en") {
                                        Some(encoding_property) => {
                                            match encoding_property.val_str() {
                                                "16" => Some(PCM16),
                                                "24" => Some(PCM24),
                                                "32" => Some(PCM32),
                                                &_ => None,
                                            }
                                        }
                                        None => None,
                                    },
                                    latency: match service_info.get_property("latency_ns") {
                                        Some(latency_property) => latency_property
                                            .val_str()
                                            .parse()
                                            .ok()
                                            .map(Duration::from_nanos),
                                        None => None,
                                    },
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

    fn make_dante_command(&mut self, command: [u8; 2], command_args: &[u8]) -> BytesMut {
        let mut buffer = bytes::BytesMut::new();
        buffer.extend_from_slice(&[0x28, 0x30]);
        assert_eq!(buffer.len(), 2);
        buffer.extend_from_slice(&((command_args.len() + 10) as u16).to_be_bytes());
        assert_eq!(buffer.len(), 4);
        buffer.extend_from_slice(&self.get_new_command_sequence_id().to_be_bytes());
        assert_eq!(buffer.len(), 6);
        buffer.extend(command);
        assert_eq!(buffer.len(), 8);
        buffer.extend_from_slice(&[0x00, 0x00]);
        assert_eq!(buffer.len(), 10);
        buffer.extend_from_slice(&command_args);
        buffer
    }

    fn send_bytes_to_addresses(
        addresses: &HashSet<Ipv4Addr>,
        port: u16,
        bytes: &[u8],
    ) -> Result<(), Box<dyn Error>> {
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        for address in addresses {
            debug!(
                "Sent bytes {:?} to {}:{}",
                hex::encode(bytes),
                address,
                port
            );
            socket.send_to(bytes, (*address, port))?;
        }
        Ok(())
    }

    fn send_bytes_to_address(
        address: &Ipv4Addr,
        port: u16,
        bytes: &[u8],
    ) -> Result<(), Box<dyn Error>> {
        let socket = UdpSocket::bind("0.0.0.0:0")?;

        debug!(
            "Sent bytes {:?} to {}:{}",
            hex::encode(bytes),
            address,
            port
        );
        socket.send_to(bytes, (*address, port))?;

        Ok(())
    }

    pub fn make_subscription(
        &mut self,
        version: &DanteVersion,
        rx_device_ip: &Ipv4Addr,
        rx_channel_id: u16,
        tx_device: &AsciiStr,
        tx_channel: &AsciiStr,
    ) -> Result<(), MakeSubscriptionError> {
        let tx_device_name_buffer = tx_device.as_bytes();
        let tx_channel_name_buffer = tx_channel.as_bytes();

        let port: u16 = 4440;

        let mut command_buffer = BytesMut::new();

        match version {
            DanteVersion::Dante4_4_1_3 => {
                command_buffer.extend_from_slice(&[
                    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x20, 0x01,
                ]);
                assert_eq!(command_buffer.len(), 10);
                command_buffer.extend_from_slice(&rx_channel_id.to_be_bytes());
                assert_eq!(command_buffer.len(), 12);
                command_buffer.extend_from_slice(&[0x00, 0x03, 0x01, 0x14]);
                assert_eq!(command_buffer.len(), 16);
                let end_pos: u16 = (276 + tx_channel_name_buffer.len() + 1) as u16;
                command_buffer.extend_from_slice(&end_pos.to_be_bytes());
                assert_eq!(command_buffer.len(), 18);
                command_buffer.extend_from_slice(&vec![0x00; 248]);
                assert_eq!(command_buffer.len(), 266);
                command_buffer.extend_from_slice(tx_channel_name_buffer);
                command_buffer.extend_from_slice(&[0x00]);
                command_buffer.extend_from_slice(tx_device_name_buffer);
                command_buffer.extend_from_slice(&[0x00]);
            }
            DanteVersion::Dante4_2_1_3 => {
                command_buffer.extend_from_slice(&[0x10, 0x01]);
                assert_eq!(command_buffer.len(), 2);
                command_buffer.extend_from_slice(&rx_channel_id.to_be_bytes());
                assert_eq!(command_buffer.len(), 4);
                command_buffer.extend_from_slice(&[0x01, 0x4C]);
                assert_eq!(command_buffer.len(), 6);
                let end_pos: u16 = (332 + tx_channel_name_buffer.len() + 1) as u16;
                command_buffer.extend_from_slice(&end_pos.to_be_bytes());
                assert_eq!(command_buffer.len(), 8);
                command_buffer.extend_from_slice(&vec![0x00; 314]);
                assert_eq!(command_buffer.len(), 322);
                command_buffer.extend_from_slice(tx_channel_name_buffer);
                command_buffer.extend_from_slice(&[0x00]);
                command_buffer.extend_from_slice(tx_device_name_buffer);
                command_buffer.extend_from_slice(&[0x00]);
            }
        }

        match Self::send_bytes_to_address(
            rx_device_ip,
            port,
            &self.make_dante_command(version.get_commands().command_subscription, &command_buffer),
        ) {
            Ok(_) => Ok(()),
            Err(_) => Err(MakeSubscriptionError::ConnectionFailed),
        }
    }

    pub fn clear_subscription(
        &mut self,
        version: &DanteVersion,
        rx_device_ip: &Ipv4Addr,
        rx_channel_id: u16,
    ) -> Result<(), MakeSubscriptionError> {
        let mut command_buffer = BytesMut::new();

        match version {
            DanteVersion::Dante4_4_1_3 => {
                command_buffer.extend_from_slice(&[
                    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x20, 0x01,
                ]);
                assert_eq!(command_buffer.len(), 10);
                command_buffer.extend_from_slice(&rx_channel_id.to_be_bytes());
                assert_eq!(command_buffer.len(), 12);
                command_buffer.extend_from_slice(&[0x00, 0x03, 0x00, 0x00, 0x00, 0x00]);
                assert_eq!(command_buffer.len(), 18);
                command_buffer.extend_from_slice(&vec![0x00; 248]);
                assert_eq!(command_buffer.len(), 266);
            }
            DanteVersion::Dante4_2_1_3 => {
                command_buffer.extend_from_slice(&[0x10, 0x01]);
                assert_eq!(command_buffer.len(), 2);
                command_buffer.extend_from_slice(&rx_channel_id.to_be_bytes());
                assert_eq!(command_buffer.len(), 4);
                command_buffer.extend_from_slice(&vec![0x00; 318]);
                assert_eq!(command_buffer.len(), 322);
            }
        }

        let port: u16 = 4440;

        match Self::send_bytes_to_address(
            rx_device_ip,
            port,
            &self.make_dante_command(version.get_commands().command_subscription, &command_buffer),
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
                    "{}:\ndbc status: {}\ncmc status: {}\narc status: {}\nchan status: {}\nid: {}\nmanufacturer: {}\nmodel: {}\nrouter_vers: {}\nrouter_info: {}\nARC port: {}\nIP: {}",
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
                    },
                    match &cache.arc_info {
                        Some(arc_info) => {format!("{:?}", &arc_info.addresses)}
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
