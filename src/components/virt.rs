use std::sync::Arc;

use anyhow::Result;
use log::debug;

use crate::{sensor::SensorUpdate, supervisor::*};
use crate::{postman::Postman, sensor::Sensors};

pub struct Virt {
    sensors: Arc<Sensors>,
    postman: Arc<Postman>,
}

impl Virt {
    pub fn new(postman: Arc<Postman>, sensors: Arc<Sensors>) -> Box<Virt> {
        Box::new(Virt { postman, sensors })
    }
}

impl ComponentFactory for Virt {
    fn create(&self) -> Result<Box<dyn Component>> {
        Ok(Box::new(VirtComponent {
            sensors: self.sensors.clone(),
            postman: self.postman.clone(),
        }))
    }
}

struct VirtComponent {
    sensors: Arc<Sensors>,
    postman: Arc<Postman>,
}

impl Component for VirtComponent {
    fn name(&self) -> String {
        "Virt".into()
    }

    fn stack_size(&self) -> Option<usize> {
        Some(1024)
    }

    fn run(&mut self) -> Result<()> {
        let recv = self.postman.subscribe::<SensorUpdate>()?;

        for update in recv.iter() {
            println!("Sensor update");
        }

        Ok(())
    }
}
