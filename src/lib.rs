use log::{debug, error, info, warn};
use mdns_sd::{ServiceDaemon, ServiceEvent};
use std::collections::{HashMap, HashSet};
use std::fmt::{Debug, Display, Formatter};
use std::net::Ipv4Addr;
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
    chan_info: Option<CHANInfo>,
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
                    chan_info: None,
                },
            );
        }

        Ok(())
    }

    fn try_add_device(&mut self, new_device_name: &str) {
        // Explicitly throw away error. If we already had one, Ok. If we make one, also Ok.
        let _ = self.add_device(new_device_name);
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
            .chan_info = Some(info);
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

pub struct DanteDeviceManager {
    device_list: Arc<Mutex<DanteDeviceList>>,
    running: Arc<Mutex<bool>>,
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

        Ok(())
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
                format!(
                    "{}:\ndbc status: {}\ncmc status: {}\narc status: {}\nchan status: {}\nid: {}\nmanufacturer: {}\nmodel: {}\nrouter_vers: {}\nrouter_info: {}",
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
                    }
                )
            })
            .collect()
    }

    pub fn new() -> Self {
        DanteDeviceManager {
            device_list: Arc::new(Mutex::new(DanteDeviceList::new())),
            running: Arc::new(Mutex::new(false)),
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
