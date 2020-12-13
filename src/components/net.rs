use std::sync::Arc;

use anyhow::Result;
use log::debug;
use tokio::net::TcpListener;
use tokio::runtime::{Builder, Runtime};

use crate::sensor::Sensors;
use crate::supervisor::*;

pub struct Net {
    sensors: Arc<Sensors>,
}

impl Net {
    pub fn new(sensors: Arc<Sensors>) -> Box<Net> {
        Box::new(Net { sensors })
    }
}

impl ComponentFactory for Net {
    fn create(&self) -> Result<Box<dyn Component>> {
        let runtime = Builder::new_current_thread().enable_all().build()?;

        Ok(Box::new(NetComponent {
            sensors: self.sensors.clone(),
            runtime,
        }))
    }
}

struct NetComponent {
    sensors: Arc<Sensors>,
    runtime: Runtime,
}

impl Component for NetComponent {
    fn name(&self) -> String {
        "Net".into()
    }

    fn stack_size(&self) -> Option<usize> {
        Some(1024)
    }

    fn run(&mut self) -> Result<()> {
        

        self.runtime.block_on(async {
            let listener = TcpListener::bind("0.0.0.0:7583").await.unwrap();

            loop {
                let (_socket, _) = listener.accept().await.unwrap();
                debug!("Got connection");
            }
        });

        Ok(())
    }
}
