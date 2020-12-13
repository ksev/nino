use std::time::{Duration, Instant};
/// Module for low-level code for actually polling hardware for sensor data
use std::{f32, sync::Arc};

use anyhow::{anyhow, Result};

use log::{debug, warn};
use rppal::{
    gpio::Level,
    gpio::{Gpio, InputPin},
    i2c::I2c,
};

use crate::{
    postman::Postman,
    sensor::{SensorUpdate, Sensors},
    supervisor::*,
};

use nb::block;

use ads1x1x::{
    channel as adc_chan, ic::Ads1115, ic::Resolution16Bit, interface::I2cInterface, mode::OneShot,
    Ads1x1x, FullScaleRange, SlaveAddr,
};

pub struct Temperature {
    sensors: Arc<Sensors>,
    postman: Arc<Postman>,
}

impl Temperature {
    pub fn new(postman: Arc<Postman>, sensors: Arc<Sensors>) -> Box<Temperature> {
        Box::new(Temperature { postman, sensors })
    }
}

impl ComponentFactory for Temperature {
    fn create(&self) -> Result<Box<dyn Component>> {
        let i2c = rppal::i2c::I2c::new()?;
        let address = SlaveAddr::default();
        let mut adc = Ads1x1x::new_ads1115(i2c, address);

        adc.set_full_scale_range(FullScaleRange::Within4_096V)
            .map_err(|err| anyhow!("{:?}", err))?;

        Ok(Box::new(TemperatureComponent {
            adc,
            sensors: self.sensors.clone(),
            postman: self.postman.clone(),
        }))
    }
}

struct TemperatureComponent {
    adc: Ads1x1x<I2cInterface<I2c>, Ads1115, Resolution16Bit, OneShot>,
    sensors: Arc<Sensors>,
    postman: Arc<Postman>,
}

impl TemperatureComponent {
    fn translate(&self, nr: u8, value: i16) -> f32 {
        let volt = value as f32;
        // Gettings the correct voltage is simply INadc * Vgain / 2^15
        // And we can constant fold the second part of that
        let volt = volt * 1.2500e-4;
        let term_res = (3.3 * 10000.0 / volt - 10000.0).round();
        let temp = crate::tenk::translate(term_res as u32);

        debug!("T{} {}V {}Ω {:.1}°C", nr, volt, term_res, temp);

        temp
    }
}

impl Component for TemperatureComponent {
    fn name(&self) -> String {
        "Temperature".into()
    }

    fn stack_size(&self) -> Option<usize> {
        Some(1024)
    }

    fn run(&mut self) -> Result<()> {
        use embedded_hal::adc::OneShot;

        loop {
            let t0 = block!(self.adc.read(&mut adc_chan::SingleA0))
                .map_err(|err| anyhow!("{:?}", err))?;

            let t0 = self.translate(0, t0);
            self.sensors["temp_0"].write(t0);
            self.postman.dispatch(SensorUpdate::new("temp_0", t0))?;

            let t1 = block!(self.adc.read(&mut adc_chan::SingleA1))
                .map_err(|err| anyhow!("{:?}", err))?;

            let t1 = self.translate(1, t1);
            self.sensors["temp_1"].write(t1);
            self.postman.dispatch(SensorUpdate::new("temp_1", t1))?;

            let t2 = block!(self.adc.read(&mut adc_chan::SingleA2))
                .map_err(|err| anyhow!("{:?}", err))?;

            let t2 = self.translate(2, t2);
            self.sensors["temp_2"].write(t2);
            self.postman.dispatch(SensorUpdate::new("temp_2", t1))?;

            let t3 = block!(self.adc.read(&mut adc_chan::SingleA3))
                .map_err(|err| anyhow!("{:?}", err))?;

            let t3 = self.translate(3, t3);
            self.sensors["temp_3"].write(t3);
            self.postman.dispatch(SensorUpdate::new("temp_3", t3))?;

            let temp = std::fs::read_to_string("/sys/class/thermal/thermal_zone0/temp")?;

            if let Ok(temp) = temp.trim().parse::<f32>() {
                let rpi = temp / 1000.0;
                self.sensors["raspberry_pi"].write(rpi);
                self.postman.dispatch(SensorUpdate::new("raspberry_pi", rpi))?;
                debug!("RPi CPU temp {:.1}°C", rpi);
            }

            std::thread::sleep(Duration::from_secs(1));
        }

        //Ok(())
    }
}

pub struct Rpm {
    sensors: Arc<Sensors>,
    postman: Arc<Postman>,
}

impl Rpm {
    pub fn new(postman: Arc<Postman>, sensors: Arc<Sensors>) -> Box<Rpm> {
        Box::new(Rpm { postman, sensors })
    }
}

impl ComponentFactory for Rpm {
    fn create(&self) -> Result<Box<dyn Component>> {
        let gpio = Gpio::new()?;

        let pin0 = gpio.get(17)?.into_input_pullup();
        let pin1 = gpio.get(27)?.into_input_pullup();

        let rs = RpmComponent {
            _gpio: gpio,
            pin0,
            pin1,
            sensors: self.sensors.clone(),
            postman: self.postman.clone(),
        };
        Ok(Box::new(rs))
    }
}

struct RpmComponent {
    _gpio: Gpio,
    pin0: InputPin,
    pin1: InputPin,
    sensors: Arc<Sensors>,
    postman: Arc<Postman>,
}

#[derive(Debug)]
enum Cluster {
    Cluster0,
    Cluster1,
}

impl Component for RpmComponent {
    fn name(&self) -> String {
        "Rpm".into()
    }

    fn stack_size(&self) -> Option<usize> {
        Some(1024)
    }

    fn run(&mut self) -> Result<()> {
        use thread_priority::*;

        let tid = thread_native_id();
        let policy = ThreadSchedulePolicy::Realtime(RealtimeThreadSchedulePolicy::Fifo);
        let params = ScheduleParams {
            sched_priority: 20 as _,
        };

        if let Err(_) = set_thread_schedule_policy(tid, policy, params) {
            warn!("Thread scheduling policy change failed");
        };

        for cluster in [Cluster::Cluster0, Cluster::Cluster1].iter().cycle() {
            let input = match cluster {
                Cluster::Cluster0 => &self.pin0,
                Cluster::Cluster1 => &self.pin1,
            };

            let mut changes = 0;
            let start = Instant::now();
            let mut before = input.read();

            for _ in 0..199 {
                // We start by doing an extra sample outside of the loop hence 199 not 200
                // This is not going to be exact on any system with a scheduler
                // but thats life, no-one is gonna die if your FAN rpm is slightly off.
                std::thread::sleep(Duration::from_millis(5));

                let current = input.read();

                if current != before {
                    // We only care if it went from high to low
                    if current == Level::Low {
                        changes += 1;
                    }
                    before = current;
                }
            }

            // try to fix the number by measuring how much time we actually sampled
            let sample_window = start.elapsed().as_secs_f32();
            let freq = changes as f32 * sample_window;
            let rpm = (freq / 2.0) * 60.0;

            debug!(
                "{:?}, RPM: {:.0}, sample_window: {}",
                cluster, rpm, sample_window
            );

            match cluster {
                Cluster::Cluster0 => {
                    self.sensors["rpm_0"].write(rpm);
                    self.postman.dispatch(SensorUpdate::new("rpm_0", rpm))?;
                }
                Cluster::Cluster1 => {
                    self.sensors["rpm_1"].write(rpm);
                    self.postman.dispatch(SensorUpdate::new("rpm_1", rpm))?;
                }
            };

            std::thread::sleep(Duration::from_secs(1));
        }

        Ok(())
    }
}
