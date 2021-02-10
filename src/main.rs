mod drop;
mod net;
mod pwm;
mod sensor;

use std::sync::Arc;

use anyhow::Result;
use clap::{App, Arg};
use drop::DropJoin;
use once_cell::sync::OnceCell;
use tokio::net::TcpListener;

use pwm::Pwm;
use sensor::{builtin::*, SensorId, SensorMessage, Sensors};

pub trait Global {
    fn global() -> &'static Self;
}

macro_rules! global {
    ($target:ty, $name:ident) => {
        static $name: OnceCell<$target> = OnceCell::new();
        impl Global for $target {
            fn global() -> &'static Self {
                $name.get().unwrap()
            }
        }
    };
}

pub const VERSION: &'static str = "0.0.1";

global!(Sensors, SENSORS);
global!(Config, CONFIG);
global!(sled::Db, DB);
global!(Workers, WORKERS);

fn main() -> Result<()> {
    env_logger::init();

    let matches = App::new("Nino")
        .version(VERSION)
        .about("Control the RPi pwm controller hat")
        .arg(
            Arg::new("name")
                .long("name")
                .short('n')
                .about("The instance name of the Nino server")
                .required(true)
                .takes_value(true),
        )
        .arg(
            Arg::new("retention")
                .short('r')
                .about("The number of sensor values the server will store for each sensor")
                .takes_value(true),
        )
        .get_matches();

    SENSORS.set(Sensors::new()).unwrap();
    CONFIG.set(Config {
        name: matches.value_of("name").unwrap().into(),
        retention: matches.value_of_t("retention").unwrap_or(100),
    }).unwrap();
    DB.set(sled::open("./settings.db")?).unwrap();
    WORKERS.set(Default::default()).unwrap();

    let workers = Workers::global();

    Sensors::global().load_saved()?;

    {
        // Start built in sensor workers
        let mut wrk = workers
            .lock()
            .expect("Could not lock sensor workers lock");

        wrk.push((
            vec![
                SensorId::Tmp0,
                SensorId::Tmp1,
                SensorId::Tmp2,
                SensorId::Tmp3,
            ],
            poll_tmp_probes()?,
        ));

        wrk.push((vec![SensorId::RPi], poll_rpi_tmp()?));
        wrk.push((vec![SensorId::RPM0, SensorId::RPM1], poll_rpm()?));
    }

    let (tx, _rx) = tokio::sync::broadcast::channel(5);
    let broadcaster = Arc::new(tx);
    let _broadcast_handle = broadcast_sensors(broadcaster.clone())?;

    let pwm = Pwm::new()?;
    pwm.set_channel0(0.6)?;
    pwm.set_channel1(0.28)?;

    let rt = tokio::runtime::Runtime::new()?;
    let _ok: Result<()> = rt.block_on(async {
        let listener = TcpListener::bind("0.0.0.0:7583").await?;

        loop {
            // The second item contains the IP and port of the new connection.
            let (socket, addr) = listener.accept().await?;

            log::debug!("{:?} connected", addr);

            let listen = broadcaster.clone();

            tokio::spawn(async move {
                match net::handle(socket, listen).await {
                    Ok(_) => return,
                    Err(e) => {
                        log::error!("Socket error:\n{:?}", e);
                        return;
                    }
                }
            });
        }
    });

    _ok
}

fn broadcast_sensors(
    broadcast: Arc<tokio::sync::broadcast::Sender<SensorMessage>>,
) -> Result<DropJoin<()>> {
    let handle = std::thread::Builder::new()
        .name("broadcaster".into())
        .stack_size(32 * 1024)
        .spawn(move || {
            let sensors = Sensors::global();

            for message in sensors.subscribe() {
                if let Err(m) = broadcast.send(message) {
                    log::error!("Error forwarding sensor data {}", m);
                }
            }

            Ok(())
        })?;

    Ok(DropJoin::new(handle))
}

#[derive(Default, Debug)]
struct Workers(std::sync::Mutex<Vec<(Vec<SensorId>, DropJoin<()>)>>);

impl std::ops::Deref for Workers {
    type Target = std::sync::Mutex<Vec<(Vec<SensorId>, DropJoin<()>)>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}


#[derive(Default, Debug)]
pub struct Config {
    pub name: String,
    pub retention: usize,
}
