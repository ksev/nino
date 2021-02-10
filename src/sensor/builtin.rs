
use std::thread;
use std::time::Duration;

use anyhow::Result;
use log::{trace, warn};

use rppal::{gpio::Gpio, gpio::Trigger, i2c::I2c};

use super::{SensorId, Sensors};
use crate::{Global, drop::DropJoin};

enum Channel {
    A0,
    A1,
    A2,
    A3,
}

pub fn poll_tmp_probes() -> Result<DropJoin<()>> {
    let mut i2c = rppal::i2c::I2c::new()?;

    // Default addres when the Adc addr pin is connection to GND
    i2c.set_slave_address(0b1001000)?;

    let handle = thread::Builder::new()
        .name("temp-sensor".into())
        .stack_size(32 * 1024)
        .spawn(move || {
            let sensors = Sensors::global();
            loop {
                let t0 = read_adc(&mut i2c, Channel::A0)?;
                sensors.set(&SensorId::Tmp0, t0);

                let t1 = read_adc(&mut i2c, Channel::A1)?;
                sensors.set(&SensorId::Tmp1, t1);

                let t2 = read_adc(&mut i2c, Channel::A2)?;
                sensors.set(&SensorId::Tmp2, t2);

                let t3 = read_adc(&mut i2c, Channel::A3)?;
                sensors.set(&SensorId::Tmp3, t3);

                thread::sleep(Duration::from_secs(1));
            }
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

pub fn poll_rpi_tmp() -> Result<DropJoin<()>> {
    let handle = thread::Builder::new()
        .name("rpi-sensor".into())
        .stack_size(32 * 1024)
        .spawn(move || {
            let sensors = Sensors::global();
            loop {
                let temp = std::fs::read_to_string("/sys/class/thermal/thermal_zone0/temp")?;
                let temp = temp.trim().parse::<f64>()?;
                let temp = temp / 1000.0;

                sensors.set(&SensorId::RPi, temp);

                thread::sleep(Duration::from_secs(3));
            }
        })?;

    Ok(DropJoin::new(handle))
}

pub fn poll_rpm() -> Result<DropJoin<()>> {
    let gpio = Gpio::new()?;

    let handle = thread::Builder::new()
        .name("rpm-counter".into())
        .stack_size(32 * 1024)
        .spawn(move || {
            let sensors = Sensors::global();

            let mut pin0 = gpio.get(17)?.into_input_pullup();
            let mut pin1 = gpio.get(27)?.into_input_pullup();

            use thread_priority::*;

            let tid = thread_native_id();
            let policy = ThreadSchedulePolicy::Realtime(RealtimeThreadSchedulePolicy::Fifo);
            let params = ScheduleParams {
                sched_priority: 20 as _,
            };

            if let Err(_) = set_thread_schedule_policy(tid, policy, params) {
                warn!("Thread scheduling policy change failed");
            };

            'outer: for cluster in [0, 1].iter().cycle() {
                let input = match cluster {
                    0 => &mut pin0,
                    1 | _ => &mut pin1,
                };

                let start = std::time::Instant::now();

                input.set_interrupt(Trigger::FallingEdge)?;

                for _ in 0..50 {
                    if let None = input.poll_interrupt(true, Some(Duration::from_secs(1)))? {
                        input.clear_interrupt()?;
                        continue 'outer;
                    }
                }

                // try to fix the number by measuring how much time we actually sampled
                let sample_window = start.elapsed().as_secs_f64();
                let freq = 50.0 / sample_window;

                input.clear_interrupt()?;

                let rpm = (freq / 2.0) * 60.0;

                trace!(
                    "{:?}, RPM: {:.0}, sample_window: {}",
                    cluster,
                    rpm,
                    sample_window
                );

                match cluster {
                    0 => sensors.set(&SensorId::RPM0, rpm),
                    1 | _ => sensors.set(&SensorId::RPM1, rpm),
                };

                std::thread::sleep(Duration::from_secs(3));
            }

            Ok(())
        })?;

    Ok(DropJoin::new(handle))
}
