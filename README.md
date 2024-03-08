# dante-control-rs

Dante discovery and control as a rust library

## Dante Versions

- [x] 4.2.1.3
- [x] 4.4.1.3

## Features

- [x] Discover Dante devices via mDNS
- [x] Make subscriptions
- [x] Clear subscriptions

## Usage

Create a new DanteDeviceManager. From there you can either poll for dante devices on the network with mdns via
start_discovery(), stop_discovery(), and get_device_names()/get_device_descriptions(), or you can control dante devices
on the network via make_subscription() and clear_subscription().