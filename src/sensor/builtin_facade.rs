use std::thread;
use std::time::Duration;

use anyhow::Result;
use rand;
use rand::Rng;

use super::{SensorId, Sensors};
use crate::{Global, drop::DropJoin};

pub fn poll_tmp_probes() -> Result<DropJoin<()>> {
    let handle = thread::Builder::new()
        .name("temp-sensor".into())
        .stack_size(32 * 1024)
        .spawn(move || {
            let mut rng = rand::thread_rng();
            let sensors = Sensors::global();

            loop {
                sensors.set(&SensorId::Tmp0, rng.gen_range(18.0..30.0));
                sensors.set(&SensorId::Tmp1, rng.gen_range(18.0..30.0));
                sensors.set(&SensorId::Tmp2, rng.gen_range(18.0..30.0));
                sensors.set(&SensorId::Tmp3, rng.gen_range(18.0..30.0));

                thread::sleep(Duration::from_secs(1));
            }
        })?;

    Ok(DropJoin::new(handle))
}

pub fn poll_rpi_tmp() -> Result<DropJoin<()>> {
    let handle = thread::Builder::new()
        .name("rpi-sensor".into())
        .stack_size(32 * 1024)
        .spawn(move || {
            let mut rng = rand::thread_rng();
            let sensors = Sensors::global();

            loop {
                sensors.set(&SensorId::RPi, rng.gen_range(20.0..50.0));

                thread::sleep(Duration::from_secs(3));
            }
        })?;

    Ok(DropJoin::new(handle))
}

pub fn poll_rpm() -> Result<DropJoin<()>> {
    let handle = thread::Builder::new()
        .name("rpm-counter".into())
        .stack_size(32 * 1024)
        .spawn(move || {
            let mut rng = rand::thread_rng();
            let sensors = Sensors::global();

            loop {
                sensors.set(&SensorId::RPM0, rng.gen_range(900.0..2400.0));
                sensors.set(&SensorId::RPM1, rng.gen_range(900.0..2400.0));

                thread::sleep(Duration::from_secs(5));
            }
        })?;

    Ok(DropJoin::new(handle))
}
