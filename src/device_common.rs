use clap::{arg, Arg, ArgMatches};

#[derive(Clone)]
pub struct DeviceCommon {
    pub device_name: String,
    pub dump_packets: bool,
    pub turn_on: bool,
    pub allow_off: bool,
}

impl DeviceCommon {
    pub fn args() -> [Arg; 4] {
        [
            arg!(--"device-name" <name>)
                .help("Provides the name of the device")
                .required(true),
            arg!(--"dump-packets").help("Dumps decoded packets to the terminal for debugging"),
            arg!(--"turn-on")
                .help("Turn on the machine before running this operation")
                .conflicts_with("allow-off"),
            arg!(--"allow-off")
                .hide(true)
                .help("Allow brewing while machine is off")
                .conflicts_with("turn-on"),
        ]
    }

    pub fn parse(cmd: &ArgMatches) -> Self {
        Self {
            device_name: cmd
                .get_one::<String>("device-name")
                .expect("Device name required")
                .clone(),
            dump_packets: cmd.get_flag("dump-packets"),
            turn_on: cmd.get_flag("turn-on"),
            allow_off: cmd.get_flag("allow-off"),
        }
    }
}
