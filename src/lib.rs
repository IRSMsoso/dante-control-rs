use log::{debug, info, warn};
use mdns_sd::{ServiceDaemon, ServiceEvent};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::Duration;

const CMC_SERVICE: &'static str = "_netaudio-cmc._udp.local.";
const DBC_SERVICE: &'static str = "_netaudio-dbc._udp.local.";
const ARC_SERVICE: &'static str = "_netaudio-arc._udp.local.";
const CHAN_SERVICE: &'static str = "_netaudio-chan._udp.local.";

const TEST_SERVICE: &str = DBC_SERVICE;

struct DanteDevice {
    name: String,
}

pub struct DanteDeviceManager {
    devices: Arc<Mutex<Vec<DanteDevice>>>,
    running: Arc<Mutex<bool>>,
}

impl DanteDeviceManager {
    /// Spawns the discovery service in a separate thread. Call stop_discovery() to end it.
    pub fn start_discovery(&self) -> Result<(), Box<dyn std::error::Error>> {
        info!("Starting discovery");
        *self.running.lock().unwrap() = true;

        let mdns = ServiceDaemon::new().expect("Failed to create mdns service daemon!");
        let receiver = mdns
            .browse(TEST_SERVICE)
            .unwrap_or_else(|_| panic!("Failed to browse for {}", TEST_SERVICE));

        // Fresh Arcs to move into thread.
        let devices = self.devices.clone();
        let running = self.running.clone();

        let thread = std::thread::spawn(move || {
            debug!("Starting discovery thread");
            while *running.lock().unwrap() {
                while let Ok(event) = receiver.try_recv() {
                    match event {
                        ServiceEvent::SearchStarted(service_name) => {
                            debug!("Search Started: {}", &service_name)
                        }
                        ServiceEvent::ServiceFound(service_name, host_service_name) => {
                            debug!("Search Found: {}, {}", &service_name, &host_service_name)
                        }
                        ServiceEvent::ServiceResolved(service_info) => {
                            info!("Service Resolved: {:?}", &service_info);
                            let device_name = service_info.get_hostname();
                            let mut devices = devices
                                .lock()
                                .expect("Cannot get mutex lock of DanteDevices");
                            for device in &*devices {
                                if device.name == device_name {
                                    return;
                                }
                            }
                            devices.push(DanteDevice::from(
                                match device_name.strip_suffix(".local.") {
                                    None => {
                                        warn!("Device \"{}\" doesn't end with \".local\". This is abnormal.", device_name);
                                        device_name
                                    },
                                    Some(stripped) => stripped,
                                },
                            ));
                        }
                        ServiceEvent::ServiceRemoved(a, b) => {
                            info!("Service Removed: {}, {}", &a, &b)
                        }
                        ServiceEvent::SearchStopped(a) => {
                            debug!("Search Stopped: {}", &a)
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
        self.devices
            .lock()
            .expect("Failed to unlock device mutex in get_device_names")
            .iter()
            .map(|device| device.name.clone())
            .collect()
    }
    pub fn new() -> Self {
        DanteDeviceManager {
            devices: Arc::new(Mutex::new(Vec::new())),
            running: Arc::new(Mutex::new(false)),
        }
    }
}

impl Default for DanteDeviceManager {
    fn default() -> Self {
        DanteDeviceManager::new()
    }
}

impl From<&str> for DanteDevice {
    fn from(value: &str) -> Self {
        Self {
            name: value.to_string(),
        }
    }
}
