use anyhow::Result;
use log::debug;
use rppal::pwm::{Channel, Polarity, Pwm as RPwm};

pub struct Pwm {
    cluster0: RPwm,
    cluster1: RPwm,
}

impl Pwm {
    pub fn new() -> Result<Pwm> {
        let cluster0 = RPwm::with_frequency(
            Channel::Pwm0, // Channel
            25_000.0,      // Frequency
            0.7,           // Duty cycle
            Polarity::Inverse,
            true, // Enabled
        )?;

        let cluster1 = RPwm::with_frequency(
            Channel::Pwm1, // Channel
            25_000.0,      // Frequency
            0.7,           // Duty cycle
            Polarity::Inverse,
            true, // Enabled
        )?;

        Ok(Pwm { cluster0, cluster1 })
    }

    pub fn set_channel0(&self, duty_cycle: f32) -> Result<()> {
        debug!("Set PWM duty cycle to {:.2} for cluster 0", duty_cycle);
        self.cluster0.set_duty_cycle(duty_cycle as f64)?;
        Ok(())
    }

    pub fn set_channel1(&self, duty_cycle: f32) -> Result<()> {
        debug!("Set PWM duty cycle to {:.2} for cluster 1", duty_cycle);
        self.cluster1.set_duty_cycle(duty_cycle as f64)?;
        Ok(())
    }
}
