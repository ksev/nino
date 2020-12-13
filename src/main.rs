/*
mod components;
mod postman;
mod pwm;
mod sensor;
mod supervisor;
mod tenk;
*/

mod pwm;

/*

use postman::Postman;
use pwm::Pwm;

use sensor::Sensors;
use supervisor::Supervisor;

use components::{Net, Rpm, Temperature, Virt};
*/

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use log::{debug, error, trace};
use rppal::{
    gpio::Level,
    gpio::{Gpio, InputPin},
    i2c::I2c,
};

use anyhow::Result;

enum Channel {
    A0,
    A1,
    A2,
    A3,
}

fn poll_tmp_probes(sensors: Arc<Sensors>) -> Result<DropJoin<()>> {
    let mut i2c = rppal::i2c::I2c::new()?;

    // Default addres when the Adc addr pin is connection to GND
    i2c.set_slave_address(0b1001000)?;

    let handle = thread::Builder::new()
        .name("temp-sensor".into())
        .stack_size(32 * 1024)
        .spawn(move || loop {
            let t0 = read_adc(&mut i2c, Channel::A0)?;
            sensors.set(&SensorId::Tmp0, t0);

            let t1 = read_adc(&mut i2c, Channel::A1)?;
            sensors.set(&SensorId::Tmp1, t1);

            let t2 = read_adc(&mut i2c, Channel::A2)?;
            sensors.set(&SensorId::Tmp2, t2);

            let t3 = read_adc(&mut i2c, Channel::A3)?;
            sensors.set(&SensorId::Tmp3, t3);

            thread::sleep(Duration::from_secs(1));
        })?;

    Ok(DropJoin::new(handle))
}

// Override Steinhart-Hart coeff
// Tool: https://www.thinksrs.com/downloads/programs/Therm%20Calc/NTCCalibrator/NTCcalculator.htm
fn read_adc(i2c: &mut I2c, channel: Channel) -> Result<f64> {
    // Only difference is which channel, so multiplex config
    // See: https://cdn-shop.adafruit.com/datasheets/ads1115.pdf for specs

    match channel {
        Channel::A0 => i2c.write(&[0b00000001, 0b11000011, 0b11100011])?,
        Channel::A1 => i2c.write(&[0b00000001, 0b11010011, 0b11100011])?,
        Channel::A2 => i2c.write(&[0b00000001, 0b11100011, 0b11100011])?,
        Channel::A3 => i2c.write(&[0b00000001, 0b11110011, 0b11100011])?,
    };

    // Wait time = nominal data period + 10%+ 20μs
    // And we're at 860SPS so 1.16ms
    thread::sleep(Duration::from_micros(1300));

    // The measured resistance of the low-side resistor in the voltage divider
    let low_side_res = 10_000.0;

    // These are standard for 10k termistors.
    // should be calibrated by end user if they know how
    let sh_a = 0.001125308852122;
    let sh_b = 0.000234711863267;
    let sh_c = 0.000000085663516;

    let mut res = [0u8; 2];
    i2c.write_read(&[0b00000000], &mut res)?;

    let value = i16::from_be_bytes(res);

    let volt = value as f64;
    // Gettings the correct voltage is simply INadc * Vgain / 2^15
    // And we can constant fold the second part of that
    let volt = volt * 1.2500e-4;
    let term_res = 3.3 * low_side_res / volt - low_side_res;

    let log_res = term_res.ln();
    let temp = 1.0 / (sh_a + sh_b * log_res + sh_c * log_res.powi(3)) - 273.15;

    Ok(temp)
}

fn poll_rpi_tmp(sensors: Arc<Sensors>) -> Result<DropJoin<()>> {
    let handle = thread::Builder::new()
        .name("rpi-sensor".into())
        .stack_size(32 * 1024)
        .spawn(move || loop {
            let temp = std::fs::read_to_string("/sys/class/thermal/thermal_zone0/temp")?;
            let temp = temp.trim().parse::<f64>()?;
            let temp = temp / 1000.0;

            sensors.set(&SensorId::RPi, temp);

            thread::sleep(Duration::from_secs(3));
        })?;

    Ok(DropJoin::new(handle))
}

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Clone, Copy)]
enum SensorId {
    Tmp0,
    Tmp1,
    Tmp2,
    Tmp3,

    RPi,

    RPM1,
    RPM2,

    Virtual(usize),
}

impl SensorId {
    fn from_usize(nr: usize) -> SensorId {
        use SensorId::*;

        match nr {
            0 => Tmp0,
            1 => Tmp1,
            2 => Tmp2,
            3 => Tmp3,

            4 => RPi,

            5 => RPM1,
            6 => RPM2,

            nr => Virtual(nr),
        }
    }

    fn to_usize(self) -> usize {
        use SensorId::*;

        match self {
            Tmp0 => 0,
            Tmp1 => 1,
            Tmp2 => 2,
            Tmp3 => 3,

            RPi => 4,

            RPM1 => 5,
            RPM2 => 6,

            Virtual(nr) => nr,
        }
    }
}

struct Sensor {
    name: String,
    value: f64,
    followers: Vec<usize>,
}

struct Sensors {
    sensor_storage: dashmap::DashMap<usize, Sensor>,

    // Subscription stuff
    park_slots: std::sync::Mutex<Vec<usize>>,
    follow_cache: dashmap::DashMap<Vec<SensorId>, (usize, usize)>,
}

impl Sensors {
    pub fn set(&self, key: &SensorId, value: f64) {
        let key = key.to_usize();

        if let Some(mut v) = self.sensor_storage.get_mut(&key) {
            trace!("Sensor {:?} = {:?}", key, value);

            v.value = value; // Set the value in the heap

            for park_id in v.followers.iter().chain(std::iter::once(&0)) {
                // Iterate trough the listeners and unpark them, 0 is always unparked special case for listening to all
                debug!("Unparking {}", park_id);
                unsafe {
                    parking_lot_core::unpark_all(*park_id, parking_lot_core::UnparkToken(key));
                }
            }
        }
    }

    pub fn follow(&self, mut keys: Vec<SensorId>) -> SensorIterator {
        keys.sort();

        let mut follow = self
            .follow_cache
            .entry(keys.clone())
            .or_insert_with(|| (self.next_slot().unwrap(), 0));
        follow.1 += 1;

        for key in keys.iter().map(|k| k.to_usize()) {
            self.sensor_storage.alter(&key, |_, mut sensor| {
                sensor.followers.push(follow.0);
                sensor
            });
        }

        debug!("New follow with id {}", follow.0);

        SensorIterator::new(follow.0, keys)
    }

    fn next_slot(&self) -> Result<usize> {
        let mut slots = self.park_slots.lock().unwrap();

        let slot = match slots.as_slice() {
            &[] => Some(1),
            &[1] => Some(2),
            list => list.windows(2).find(|s| s[1] != s[0] + 1).map(|s| s[0] + 1),
        };

        if let Some(slot) = slot {
            slots.insert(slot - 1, slot);
            Ok(slot)
        } else {
            Err(anyhow::anyhow!("Could not find a new park slot"))
        }
    }
}

struct SensorIterator {
    id: usize,
    sensors: Vec<SensorId>,
}

impl SensorIterator {
    pub fn new(id: usize, sensors: Vec<SensorId>) -> SensorIterator {
        SensorIterator { id, sensors }
    }
}

impl Iterator for SensorIterator {
    type Item = SensorId;

    fn next(&mut self) -> Option<Self::Item> {
        debug!("Parking {}", self.id);

        let res = unsafe {
            parking_lot_core::park(
                self.id,
                || true,
                || {},
                |_, _| {},
                parking_lot_core::DEFAULT_PARK_TOKEN,
                None,
            )
        };

        match res {
            parking_lot_core::ParkResult::Unparked(parking_lot_core::UnparkToken(token)) => Some(SensorId::from_usize(token)),
            _ => None,
        }
    }
}

impl Default for Sensors {
    fn default() -> Self {
        let sensor_storage = dashmap::DashMap::new();

        sensor_storage.insert(
            0,
            Sensor {
                name: "Tmp0".into(),
                value: 0.0,
                followers: vec![],
            },
        );
        sensor_storage.insert(
            1,
            Sensor {
                name: "Tmp1".into(),
                value: 0.0,
                followers: vec![],
            },
        );
        sensor_storage.insert(
            2,
            Sensor {
                name: "Tmp2".into(),
                value: 0.0,
                followers: vec![],
            },
        );
        sensor_storage.insert(
            3,
            Sensor {
                name: "Tmp3".into(),
                value: 0.0,
                followers: vec![],
            },
        );

        sensor_storage.insert(
            4,
            Sensor {
                name: "RPi".into(),
                value: 0.0,
                followers: vec![],
            },
        );

        sensor_storage.insert(
            5,
            Sensor {
                name: "RPM1".into(),
                value: 0.0,
                followers: vec![],
            },
        );
        sensor_storage.insert(
            6,
            Sensor {
                name: "RPM2".into(),
                value: 0.0,
                followers: vec![],
            },
        );

        Sensors {
            sensor_storage,
            park_slots: std::sync::Mutex::new(vec![]),
            follow_cache: dashmap::DashMap::new(),
        }
    }
}

struct DropJoin<T> {
    handle: Option<std::thread::JoinHandle<Result<T>>>,
}

impl<T> DropJoin<T> {
    pub fn new(handle: std::thread::JoinHandle<Result<T>>) -> DropJoin<T> {
        DropJoin {
            handle: Some(handle),
        }
    }
}

impl<T> Drop for DropJoin<T> {
    fn drop(&mut self) {
        if let Some(inner) = self.handle.take() {
            let res = inner
                .join()
                .map_err(|e| anyhow::format_err!("{:?}", e))
                .and_then(|r| r);
            if res.is_err() && !std::thread::panicking() {
                res.unwrap();
            }
        }
    }
}

fn main() -> Result<()> {
    env_logger::init();

    let sensors = Arc::new(Sensors::default());

    let _tmp_handle = poll_tmp_probes(sensors.clone())?;
    let _rpi_handle = poll_rpi_tmp(sensors.clone())?;

    let iter0 = sensors.follow(vec![SensorId::Tmp0, SensorId::RPi]);

    for v in iter0 {
        println!("{:?}", v);
    }

    /*
    let temp = Temp::register(sensors.clone());
    let temp_handle = temp.start()?;
    */

    // sensors.get("tmp0");
    //let tmp1 = sensors.get("tmp1").unwrap().clone();
    //let tmp2 = sensors.get("tmp2").unwrap().clone();

    //temp_handle.join().unwrap().unwrap();

    //x2.join().unwrap().unwrap();

    /*
    let responder = libmdns::Responder::new().unwrap();

    let txt = format!("Mr Freeze|{}", 7);
    let _svc = responder.register(
        "_nino._tcp".to_owned(),
        "mrfreeze".to_owned(),
        7583,
        &[&txt],
    );*/
    /*
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    rt.block_on(async {
        let listener = TcpListener::bind("0.0.0.0:7583").await.unwrap();

        let mut invalidate_cheap = interval(Duration::from_secs(1));
        let mut invalidate_expensive = interval(Duration::from_secs(3));
        let mut duty_cycle = interval(Duration::from_millis(500));

        loop {
            tokio::select! {
                _ = invalidate_expensive.next() => {
                    use Sensor::*;
                    let mut query = SensorQuery.in_db_mut(db.as_mut());

                    query.invalidate(&Rpm0);
                    query.invalidate(&Rpm1);
                    query.invalidate(&Rpi);

                    let query = SensorQuery.in_db(db.as_ref());
                    query.sweep(SweepStrategy::discard_outdated());
                },
                _ = invalidate_cheap.next() => {
                    use Sensor::*;
                    let mut query = SensorQuery.in_db_mut(db.as_mut());

                    query.invalidate(&Temp0);
                    query.invalidate(&Temp1);
                    query.invalidate(&Temp2);
                    query.invalidate(&Temp3);

                    let query = SensorQuery.in_db(db.as_ref());
                    query.sweep(SweepStrategy::discard_outdated());
                },
                _ = duty_cycle.next() => {
                    let pwm0 = db.duty_cycle(0);
                    println!("Duty cycle {}", pwm0);
                },
                _socket = listener.accept() => {
                    println!("got socket");
                }
            };
        }
    });*/

    /*let pwm = pwm::Pwm::new().unwrap();

    loop {
        let mut buffer = String::new();
        std::io::stdin().read_line(&mut buffer).unwrap();
        pwm.set_channel0(buffer.trim().parse().unwrap()).unwrap();
    }*/

    Ok(())
}
