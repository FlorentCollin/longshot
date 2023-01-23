#![warn(clippy::all)]
use longshot::device_common::DeviceCommon;
use longshot::mqtt::{AwsConfig, MqttServer};
use std::fs::{self, File};
use std::io::Read;
use std::str;

use std::sync::Arc;

use clap::builder::{PossibleValue, PossibleValuesParser};
use clap::{arg, command};

mod app;

use longshot::ecam::{ecam, ecam_scan, get_ecam_simulator, pipe_stdin, EcamBT};
use longshot::{operations::*, protocol::*};

fn enum_value_parser<T: MachineEnumerable<T> + 'static>() -> PossibleValuesParser {
    PossibleValuesParser::new(T::all().map(|x| PossibleValue::new(x.to_arg_string())))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    pretty_env_logger::init();
    longshot::display::initialize_display();

    let matches = command!()
        .arg(arg!(--"trace").help("Trace packets to/from device"))
        .subcommand(
            command!("brew")
                .about("Brew a coffee")
                .args(&DeviceCommon::args())
                .arg(
                    arg!(--"beverage" <name>)
                        .required(true)
                        .help("The beverage to brew")
                        .value_parser(enum_value_parser::<EcamBeverageId>()),
                )
                .arg(
                    arg!(--"coffee" <amount>)
                        .help("Amount of coffee to brew")
                        .value_parser(0..=2500),
                )
                .arg(
                    arg!(--"milk" <amount>)
                        .help("Amount of milk to steam/pour")
                        .value_parser(0..=2500),
                )
                .arg(
                    arg!(--"hotwater" <amount>)
                        .help("Amount of hot water to pour")
                        .value_parser(0..=2500),
                )
                .arg(
                    arg!(--"taste" <taste>)
                        .help("The strength of the beverage")
                        .value_parser(enum_value_parser::<EcamBeverageTaste>()),
                )
                .arg(
                    arg!(--"temperature" <temperature>)
                        .help("The temperature of the beverage")
                        .value_parser(enum_value_parser::<EcamTemperature>()),
                )
                .arg(
                    arg!(--"allow-defaults")
                        .help("Allow brewing if some parameters are not specified"),
                )
                .arg(arg!(--"force").help("Allow brewing with parameters that do not validate"))
                .arg(
                    arg!(--"skip-brew")
                        .hide(true)
                        .help("Does everything except actually brew the beverage"),
                ),
        )
        .subcommand(
            command!("monitor")
                .about("Monitor the status of the device")
                .args(&DeviceCommon::args()),
        )
        .subcommand(
            command!("read-parameter")
                .about("Read a parameter from the device")
                .args(&DeviceCommon::args())
                .arg(arg!(--"parameter" <parameter>).help("The parameter ID"))
                .arg(arg!(--"length" <length>).help("The parameter length")),
        )
        .subcommand(
            command!("list-recipes")
                .about("List recipes stored in the device")
                .args(&DeviceCommon::args())
                .arg(arg!(--"detail").help("Show detailed ingredient information"))
                .arg(arg!(--"raw").help("Show raw ingredient information")),
        )
        .subcommand(
            command!("server")
                .about("Launch an MQTT listener to brew coffee")
                .args(&DeviceCommon::args())
                .arg(arg!(--"ca" <ca>).help("The certificate authority"))
                .arg(arg!(--"client-cert" <client_cert>).help("The client ceritificate"))
                .arg(arg!(--"client-key" <client_key>).help("The client private key"))
                .arg(
                    arg!(--"client-id" <client_id>)
                        .help("The client id to use when publishing on MQTT"),
                )
                .arg(arg!(--"endpoint" <endpoint>).help("The MQTT endpoint"))
                .arg(
                    arg!(--"listen-topic" <listen_topic>)
                        .help("The topic on which the MQTT client listens for requests"),
                ),
        )
        .subcommand(command!("list").about("List all supported devices"))
        .subcommand(
            command!("x-internal-pipe")
                .about("Used to communicate with the device")
                .hide(true)
                .args(&DeviceCommon::args()),
        )
        .get_matches();

    if matches.get_flag("trace") {
        longshot::logging::enable_tracing();
    }

    let subcommand = matches.subcommand();
    match subcommand {
        Some(("brew", cmd)) => {
            let skip_brew = cmd.get_flag("skip-brew");
            let allow_defaults = cmd.get_flag("allow-defaults");
            let force = cmd.get_flag("force");

            let beverage: EcamBeverageId = EcamBeverageId::lookup_by_name_case_insensitive(
                cmd.get_one::<String>("beverage").unwrap(),
            )
            .expect("Beverage required");

            let mut ingredients = vec![];
            for arg in ["coffee", "milk", "hotwater", "taste", "temperature"] {
                if let Some(value) = cmd.get_raw(arg) {
                    // Once clap has had a chance to validate the args, we go back to the underlying OsStr to parse it
                    let value = value.into_iter().next().unwrap().to_str().unwrap();
                    if let Some(ingredient) = BrewIngredientInfo::from_arg(arg, value) {
                        ingredients.push(ingredient);
                    } else {
                        eprintln!("Invalid value '{}' for argument '{}'", value, arg);
                        return Ok(());
                    }
                }
            }

            let mode = match (allow_defaults, force) {
                (_, true) => IngredientCheckMode::Force,
                (true, false) => IngredientCheckMode::AllowDefaults,
                (false, false) => IngredientCheckMode::Strict,
            };
            let device_common = DeviceCommon::parse(cmd);
            let ecam = ecam(&device_common, false).await?;
            let recipe = validate_brew(ecam.clone(), beverage, ingredients, mode).await?;
            brew(ecam.clone(), skip_brew, beverage, recipe).await?;
        }
        Some(("monitor", cmd)) => {
            let device_common = DeviceCommon::parse(cmd);
            let ecam = ecam(&device_common, true).await?;
            monitor(ecam).await?;
        }
        Some(("list", _cmd)) => {
            let (s, uuid) = ecam_scan().await?;
            longshot::info!("{}  {}", s, uuid);
        }
        Some(("list-recipes", cmd)) => {
            let device_common = DeviceCommon::parse(cmd);
            let ecam = ecam(&device_common, true).await?;
            let detailed = cmd.get_flag("detail");
            let raw = cmd.get_flag("raw");
            if detailed {
                list_recipes_detailed(ecam).await?;
            } else if raw {
                list_recipes_raw(ecam).await?;
            } else {
                list_recipes(ecam).await?;
            }
        }
        Some(("read-parameter", cmd)) => {
            let parameter = cmd
                .get_one::<String>("parameter")
                .map(|s| s.parse::<u16>().expect("Invalid number"))
                .expect("Required");
            let length = cmd
                .get_one::<String>("length")
                .map(|s| s.parse::<u8>().expect("Invalid number"))
                .expect("Required");
            let device_common = DeviceCommon::parse(cmd);
            let ecam = ecam(&device_common, true).await?;
            read_parameter(ecam, parameter, length).await?;
        }
        Some(("x-internal-pipe", cmd)) => {
            let device_name = DeviceCommon::parse(cmd).device_name;
            if device_name.starts_with("sim") {
                let ecam = Arc::new(Box::new(get_ecam_simulator(&device_name).await?));
                pipe_stdin(&ecam).await?;
            } else {
                let ecam = Arc::new(Box::new(EcamBT::get(device_name).await?));
                pipe_stdin(&ecam).await?;
            }
        }
        Some(("server", cmd)) => {
            let ca = cmd
                .get_one::<String>("ca")
                .expect("The argument ca must be specified")
                .clone();
            let client_cert = cmd
                .get_one::<String>("client-cert")
                .expect("The argument client-cert must be specified")
                .clone();
            let client_key = cmd
                .get_one::<String>("client-key")
                .expect("The argument client-key must be specified")
                .clone();
            let endpoint = cmd
                .get_one::<String>("endpoint")
                .expect("The argument endpoint must be specified")
                .clone();
            let listen_topic = cmd
                .get_one::<String>("listen-topic")
                .expect("The argument listen-topic must be specified")
                .clone();
            let client_id = cmd
                .get_one::<String>("client-id")
                .expect("The argument client-id must be specified")
                .clone();
            let mqtt_server = MqttServer {
                aws_config: AwsConfig {
                    ca: get_file_as_byte_vec(&ca),
                    client_cert: get_file_as_byte_vec(&client_cert),
                    client_key: get_file_as_byte_vec(&client_key),
                },
                client_id,
                listen_topic,
                endpoint,
            };
            let device_common = DeviceCommon::parse(cmd);
            mqtt_server.launch_server(device_common).await?;
        }
        _ => {}
    }

    longshot::display::shutdown();
    Ok(())
}

fn get_file_as_byte_vec(filename: &String) -> Vec<u8> {
    let mut f = File::open(&filename).expect("no file found");
    let metadata = fs::metadata(&filename).expect("unable to read metadata");
    let mut buffer = vec![0; metadata.len() as usize];
    f.read(&mut buffer).expect("buffer overflow");

    buffer
}
