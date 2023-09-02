use futures_util::{pin_mut, stream::StreamExt};
use mdns::{Error, Record, RecordKind};
use std::sync::Arc;
use std::{net::IpAddr, time::Duration};
use tokio::main;

/// The hostname of the devices we are searching for.
/// Every Chromecast will respond to the service name in this example.
const DBC_SERVICE: &'static str = "_netaudio-dbc._udp.local";

struct DanteDevices {
    devices: Arc<Vec<DanteDevice>>,
}

impl DanteDevices {
    async fn start_discovery(&self, mdns_query_interval: Duration) -> Result<(), Error> {
        let stream = mdns::discover::all(DBC_SERVICE, mdns_query_interval)?.listen();

        pin_mut!(stream);

        let devices: Vec<String> = Vec::new();

        while let Some(Ok(response)) = stream.next().await {
            let a: Vec<Record> = response.answers;
            println!("{:?}", a);
            println!("==================================")
        }

        Ok(())
    }

    fn new() -> Self {
        DanteDevices {
            devices: Arc::new(Vec::new()),
        }
    }
}

#[main]
async fn main() -> Result<(), Error> {
    // Iterate through responses from each Cast device, asking for new devices every 15s

    let dante_devices = DanteDevices::new();
    dante_devices
        .start_discovery(Duration::from_secs(2))
        .await?;

    Ok(())
}

fn to_ip_addr(record: &Record) -> Option<IpAddr> {
    match record.kind {
        RecordKind::A(addr) => Some(addr.into()),
        RecordKind::AAAA(addr) => Some(addr.into()),
        _ => None,
    }
}

struct DanteDevice {
    name: String,
}
