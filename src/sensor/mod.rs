use std::{cell::RefCell, collections::{HashSet, VecDeque}, convert::TryInto, rc::Rc, thread, time::Instant};

#[cfg(target_arch = "arm")]
pub mod builtin;

#[cfg(not(target_arch = "arm"))]
pub mod builtin_facade;

#[cfg(not(target_arch = "arm"))]
pub use builtin_facade as builtin;

use crossbeam_channel::TrySendError;
use rhai::RegisterResultFn;
use serde::{Deserialize, Serialize};

use anyhow::Result;
use log;

use crate::{Global, Workers, drop::DropJoin, Config};

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Clone, Copy)]
pub enum SensorId {
    Tmp0,
    Tmp1,
    Tmp2,
    Tmp3,

    RPi,

    RPM0,
    RPM1,

    Virtual(usize),
}

impl SensorId {
    pub fn from_usize(nr: usize) -> SensorId {
        use SensorId::*;

        match nr {
            0 => Tmp0,
            1 => Tmp1,
            2 => Tmp2,
            3 => Tmp3,

            4 => RPi,

            5 => RPM0,
            6 => RPM1,

            nr => Virtual(nr),
        }
    }

    pub fn to_usize(self) -> usize {
        use SensorId::*;

        match self {
            Tmp0 => 0,
            Tmp1 => 1,
            Tmp2 => 2,
            Tmp3 => 3,

            RPi => 4,

            RPM0 => 5,
            RPM1 => 6,

            Virtual(nr) => nr,
        }
    }

    pub fn to_be_bytes(self) -> [u8; std::mem::size_of::<usize>()] {
        self.to_usize().to_be_bytes()
    }

    pub fn is_virtual(&self) -> bool {
        return match self {
            SensorId::Virtual(_) => true,
            _ => false,
        };
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Sensor {
    pub alias: String,
    #[serde(skip)]
    pub values: VecDeque<f64>,
    pub unit: String,
    pub rate: usize,

    pub source: Option<String>,

    #[serde(skip)]
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum SensorMessage {
    Remove(SensorId),
    Config(SensorId),
    Update(SensorId, f64),
    Error(SensorId),
    ClearError(SensorId),
}

#[derive(Debug)]
pub struct Sensors {
    sensor_storage: dashmap::DashMap<SensorId, Sensor>,
    // Subscription stuff
    followers: std::sync::Mutex<Vec<crossbeam_channel::Sender<SensorMessage>>>,
}

impl Sensors {
    pub fn new() -> Sensors {
        Sensors {
            sensor_storage: dashmap::DashMap::new(),
            followers: std::sync::Mutex::new(vec![]),
        }
    }

    pub fn load_saved(&self) -> Result<()> {
        let config = Config::global();
        let database = sled::Db::global();

        let builtin = database.open_tree("sensor-builtin")?;

        let tmp0 = builtin
            .get(SensorId::Tmp0.to_be_bytes())?
            .and_then(|data| bincode::deserialize(&data).ok())
            .unwrap_or_else(|| Sensor {
                alias: "Tmp0".into(),
                values: VecDeque::with_capacity(config.retention),
                unit: "°C".into(),
                rate: 1000,
                source: None,
                error: None,
            });

        self.sensor_storage.insert(SensorId::Tmp0, tmp0);

        let tmp1 = builtin
            .get(SensorId::Tmp1.to_be_bytes())?
            .and_then(|data| bincode::deserialize(&data).ok())
            .unwrap_or_else(|| Sensor {
                alias: "Tmp1".into(),
                values: VecDeque::with_capacity(config.retention),
                unit: "°C".into(),
                rate: 1000,
                source: None,
                error: None,
            });

        self.sensor_storage.insert(SensorId::Tmp1, tmp1);

        let tmp2 = builtin
            .get(SensorId::Tmp2.to_be_bytes())?
            .and_then(|data| bincode::deserialize(&data).ok())
            .unwrap_or_else(|| Sensor {
                alias: "Tmp2".into(),
                values: VecDeque::with_capacity(config.retention),
                unit: "°C".into(),
                rate: 1000,
                source: None,
                error: None,
            });

        self.sensor_storage.insert(SensorId::Tmp2, tmp2);

        let tmp3 = builtin
            .get(SensorId::Tmp3.to_be_bytes())?
            .and_then(|data| bincode::deserialize(&data).ok())
            .unwrap_or_else(|| Sensor {
                alias: "Tmp3".into(),
                values: VecDeque::with_capacity(config.retention),
                unit: "°C".into(),
                rate: 1000,
                source: None,
                error: None,
            });

        self.sensor_storage.insert(SensorId::Tmp3, tmp3);

        let rpi = builtin
            .get(SensorId::RPi.to_be_bytes())?
            .and_then(|data| bincode::deserialize(&data).ok())
            .unwrap_or_else(|| Sensor {
                alias: "RPi".into(),
                values: VecDeque::with_capacity(config.retention),
                unit: "°C".into(),
                rate: 3000,
                source: None,
                error: None,
            });

        self.sensor_storage.insert(SensorId::RPi, rpi);

        let rpm0 = builtin
            .get(SensorId::RPM0.to_be_bytes())?
            .and_then(|data| bincode::deserialize(&data).ok())
            .unwrap_or_else(|| Sensor {
                alias: "Rpm0".into(),
                values: VecDeque::with_capacity(config.retention),
                unit: "RPM".into(),
                rate: 6000,
                source: None,
                error: None,
            });

        self.sensor_storage.insert(SensorId::RPM0, rpm0);

        let rpm1 = builtin
            .get(SensorId::RPM1.to_be_bytes())?
            .and_then(|data| bincode::deserialize(&data).ok())
            .unwrap_or_else(|| Sensor {
                alias: "Rpm1".into(),
                values: VecDeque::with_capacity(config.retention),
                unit: "RPM".into(),
                rate: 6000,
                source: None,
                error: None,
            });

        self.sensor_storage.insert(SensorId::RPM1, rpm1);

        let virt = database.open_tree("sensor-virtual")?;

        for res in virt.iter() {
            let (key, value) = res?;

            let id: &[u8] = &key;
            let id = usize::from_be_bytes(id.try_into()?);
            let id = SensorId::from_usize(id);

            let sensor = bincode::deserialize(&value)?;

            self.sensor_storage.insert(id, sensor);
            start_virtual_worker(id);
        }

        Ok(())
    }

    fn next_virt_id(&self) -> SensorId {
        let max = self
            .sensor_storage
            .iter()
            .map(|r| r.key().to_usize())
            .max()
            .unwrap_or(6);

        SensorId::Virtual(max + 1)
    }

    pub fn add_virtual(&self) {
        let id = self.next_virt_id();
        let sensor = Sensor {
            alias: format!("{:?}", id),
            unit: "?".into(),
            values: Default::default(),
            rate: 1000,
            source: Some("sensor(0)".into()),
            error: None,
        };

        if let Err(m) = self.save_sensor(&id, &sensor) {
            log::error!("Saving sensor config to disk failed {}", m);
        }

        self.sensor_storage.insert(id, sensor);

        log::trace!("Added sensor {:?}", id);

        self.broadcast(SensorMessage::Config(id));

        start_virtual_worker(id);
    }

    pub fn set_error(&self, key: &SensorId, error: String) {
        if let Some(mut s) = self.sensor_storage.get_mut(key) {
            let e = Some(error);

            if s.error != e {
                s.error = e;
                self.broadcast(SensorMessage::Error(*key));
            }
        }
    }

    pub fn clear_error(&self, key: &SensorId) {
        if let Some(mut s) = self.sensor_storage.get_mut(key) {
            s.error = None;
            self.broadcast(SensorMessage::ClearError(*key));
        }
    }

    pub fn reconfigure(&self, key: &SensorId, alias: String, unit: String, rate: Option<usize>, source: Option<String>) {
        log::trace!(
            "Reconfig {:?}, alias={}, unit={}",
            key,
            alias,
            unit,
        );

        if let Some(mut s) = self.sensor_storage.get_mut(key) {
            s.alias = alias;
            s.unit = unit;

            if key.is_virtual() {
                s.rate = rate.unwrap_or(1000);
                s.source = source;
            }

            if let Err(m) = self.save_sensor(key, &s) {
                log::error!("Saving sensor config to disk failed {}", m);
            }

            self.broadcast(SensorMessage::Config(*key));
        }
    }

    pub fn save_sensor(&self, key: &SensorId, sensor: &Sensor) -> Result<()> {
        let database = sled::Db::global();
        let data = bincode::serialize(&sensor)?;

        if key.is_virtual() {
            let tree = database.open_tree("sensor-virtual")?;
            tree.insert(key.to_be_bytes(), data)?;
        } else {
            let tree = database.open_tree("sensor-builtin")?;
            tree.insert(key.to_be_bytes(), data)?;
        }

        Ok(())
    }

    pub fn set(&self, key: &SensorId, value: f64) {
        if value < 0.0 || value.is_nan() {
            return; // Negative values are not real
        }

        if let Some(mut sensor) = self.sensor_storage.get_mut(&key) {
            let retention = Config::global().retention;

            log::trace!("Sensor {:?} = {:?}", key, value);

            if sensor.values.len() >= retention {
                sensor.values.pop_back();
            }

            sensor.values.push_front(value); // Set the value in the heap

            self.broadcast(SensorMessage::Update(*key, value));
        }
    }

    pub fn get(&self, key: &SensorId) -> Option<dashmap::mapref::one::Ref<'_, SensorId, Sensor>> {
        self.sensor_storage.get(key)
    }

    pub fn get_value(&self, key: &SensorId) -> Option<f64> {
        self.sensor_storage
            .get(key)
            .and_then(|s| s.values.front().copied())
    }

    fn broadcast(&self, message: SensorMessage) {
        let mut all = self
            .followers
            .lock()
            .expect("Read rwlock for all followers");

        all.retain(|chan| {
            if let Err(TrySendError::Disconnected(_)) = chan.try_send(message.clone()) {
                log::info!("Clean up disconnected follower");
                return false;
            }
            return true;
        });
    }

    pub fn iter(&self) -> dashmap::iter::Iter<'_, SensorId, Sensor> {
        self.sensor_storage.iter()
    }

    pub fn subscribe(&self) -> SensorIterator {
        let (tx, rx) = crossbeam_channel::bounded(25);

        let mut list = self
            .followers
            .lock()
            .expect("to write lock followers all lock");

        list.push(tx.clone());

        SensorIterator::new(rx)
    }
}

pub struct SensorIterator {
    rx: crossbeam_channel::Receiver<SensorMessage>,
}

impl<'a> SensorIterator {
    pub fn new(rx: crossbeam_channel::Receiver<SensorMessage>) -> SensorIterator {
        SensorIterator { rx }
    }
}

impl Iterator for SensorIterator {
    type Item = SensorMessage;

    fn next(&mut self) -> Option<Self::Item> {
        self.rx.recv().ok()
    }
}

fn start_virtual_worker(id: SensorId) {
    let handle = thread::spawn(move || {
        let sensors = Sensors::global();
        let dependecies = Rc::new(RefCell::new(HashSet::new()));

        let mut eng = rhai::Engine::new();
        let mut rate = sensors.get(&id).map(|s| s.rate).unwrap_or(1000) as u128;

        let deps = dependecies.clone();

        eng.register_result_fn("sensor", move |index: i32| {
            let id = SensorId::from_usize(index as usize);
            let sensors = Sensors::global();

            let res = match sensors.get_value(&id) {
                Some(val) => Ok(val.into()),
                None => return Err(format!("Could not find {:?}", id).into()),
            };

            deps.borrow_mut().insert(id);

            res
        });

        let mut compiled = {
            match sensors.get(&id).and_then(|s| s.source.clone()) {
                Some(ref src) => eng.compile(src),
                None => eng.compile("N/A"),
            }
        };

        let mut last = Instant::now();

        'worker: for upd in sensors.subscribe() {
            match upd {
                SensorMessage::Remove(s) if s == id => {
                    log::debug!("Sensor {:?} removed, killing worker", id);
                    break 'worker;
                } // Client removed the sensor, let the worker die
                SensorMessage::Config(s) if s == id => {
                    if let Some(sensor) = sensors.get(&id) {
                        rate = sensor.rate as u128;
                        compiled = match sensor.source {
                            Some(ref src) => eng.compile(src),
                            None => eng.compile("N/A"),
                        };

                        // We dont know which depedencies are now in play
                        dependecies.borrow_mut().clear();

                        // The user might have fixed the error
                        log::debug!("Recompile {:?} source", id);
                    }

                    sensors.clear_error(&id);
                }
                SensorMessage::Update(s, _value)
                    if (dependecies.borrow().contains(&s)
                        || dependecies.borrow().is_empty())
                        && last.elapsed().as_millis() > rate =>
                {
                    dependecies.borrow_mut().clear();

                    let ast = match compiled {
                        Ok(ref ast) => ast,
                        Err(ref e) => {
                            sensors.set_error(&id, format!("{:?}", e));
                            continue 'worker;
                        }
                    };

                    // Run script and update this sensors value
                    match eng.eval_ast(ast) {
                        Ok(value) => {
                            sensors.set(&id, value);
                            last = Instant::now();
                        }
                        Err(e) => {
                            sensors.set_error(&id, format!("{:?}", e));
                            continue 'worker;
                        }
                    }
                }

                _ => { /* The other cases we can safely ignore */ }
            }
        }

        Ok(())
    });

    let mut wrk = Workers::global().lock().expect("Cant lock sensor workers");
    wrk.push((vec![id], DropJoin::new(handle)));
}

