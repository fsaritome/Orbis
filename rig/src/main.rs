//! `rig`: drive the Orbis turntable from the command line (no camera needed).

use std::thread::sleep;
use std::time::Duration;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use rig::{Geometry, Rig};

#[derive(Parser)]
#[command(about = "Control the Orbis photogrammetry turntable")]
struct Cli {
    /// Turntable IP address (Wi-Fi). Found automatically if omitted.
    #[arg(long, global = true, conflicts_with = "port")]
    host: Option<String>,
    /// Use USB serial instead of Wi-Fi, e.g. COM5.
    #[arg(long, global = true)]
    port: Option<String>,
    /// Use USB serial and auto-detect the port.
    #[arg(long, global = true, conflicts_with_all = ["host", "port"])]
    usb: bool,
    /// Full steps per motor revolution.
    #[arg(long, global = true, default_value_t = 200)]
    motor_steps: u32,
    /// Driver microstepping (ST-4045 DIP SW4–6; 1/16 → 16).
    #[arg(long, global = true, default_value_t = 16)]
    microsteps: u32,
    /// Motor turns per turntable turn (1 = direct drive).
    #[arg(long, global = true, default_value_t = 1.0)]
    gear_ratio: f64,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List serial ports.
    Ports,
    /// Look for turntables on the network.
    Discover,
    /// Store Wi-Fi credentials on the turntable (normally over --usb).
    Wifi {
        ssid: String,
        /// Leave out for an open network.
        #[arg(long, default_value = "")]
        password: String,
    },
    /// Show the turntable's network state.
    Netinfo,
    /// Check the firmware answers.
    Ping,
    /// Show position, speed and enable state.
    Status,
    /// Declare the current position as 0°.
    Zero,
    /// Turn by a relative angle in degrees (negative = backwards).
    Rotate {
        #[arg(allow_hyphen_values = true)]
        degrees: f64,
    },
    /// Turn to an absolute angle in degrees.
    Goto {
        #[arg(allow_hyphen_values = true)]
        degrees: f64,
    },
    /// Move by raw motor steps.
    Steps {
        #[arg(allow_hyphen_values = true)]
        steps: i32,
    },
    /// Stop a running move.
    Stop,
    /// Energise the motor.
    Enable,
    /// Release the motor (it can be turned by hand).
    Disable,
    /// Set top speed (steps/s) and acceleration (steps/s²).
    Speed { max_speed: u32, accel: u32 },
    /// Dry-run a scan: turn in N equal steps with a pause at each.
    Spin {
        #[arg(long, default_value_t = 24)]
        shots: u32,
        #[arg(long, default_value_t = 500)]
        pause_ms: u64,
    },
    /// Print button presses until Ctrl+C.
    Listen,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let geo = Geometry { motor_steps: cli.motor_steps, microsteps: cli.microsteps, gear_ratio: cli.gear_ratio };

    match cli.cmd {
        Cmd::Ports => {
            for (name, desc) in rig::list_serial_ports()? {
                println!("{name:8} {desc}");
            }
            return Ok(());
        }
        Cmd::Discover => {
            let found = rig::discover(Duration::from_secs(2))?;
            if found.is_empty() {
                bail!("no turntable answered (is it on Wi-Fi? `rig --usb netinfo` shows its state)");
            }
            for f in found {
                println!("{f}");
            }
            return Ok(());
        }
        _ => {}
    }

    let mut rig = if let Some(host) = &cli.host {
        Rig::connect_host(host)?
    } else if let Some(port) = &cli.port {
        Rig::open_serial(port)?
    } else if cli.usb {
        Rig::open_serial(&rig::find_serial_port()?)?
    } else {
        Rig::open_auto()?
    };
    eprintln!("connected via {}", rig.description());

    let report = |rig: &mut Rig| -> Result<()> {
        let s = rig.status()?;
        println!("position: {} steps ({:.2}°)", s.position, geo.steps_to_degrees(s.position));
        Ok(())
    };

    match cli.cmd {
        Cmd::Ports | Cmd::Discover => unreachable!(),
        Cmd::Wifi { ssid, password } => {
            rig.set_wifi(&ssid, &password)?;
            println!("saved; the turntable is restarting and joining \"{ssid}\"");
            println!("check with `rig discover` in ~10 s, or `rig --usb netinfo`");
        }
        Cmd::Netinfo => {
            let n = rig.net_info()?;
            if n.ssid.is_empty() {
                println!("no Wi-Fi configured (set it with `rig --usb wifi <ssid> --password <pw>`)");
            } else if n.connected {
                println!("connected to \"{}\" as {}", n.ssid, n.ip);
            } else {
                println!("configured for \"{}\" but not connected (wrong password? out of range?)", n.ssid);
            }
        }
        Cmd::Ping => println!("turntable firmware answered"),
        Cmd::Status => {
            let s = rig.status()?;
            println!("position : {} steps ({:.2}°)", s.position, geo.steps_to_degrees(s.position));
            println!("moving   : {}", s.moving);
            println!("enabled  : {}", s.enabled);
            println!("max speed: {} steps/s ({:.1}°/s)", s.max_speed, geo.steps_to_degrees(s.max_speed as i32));
            println!("accel    : {} steps/s²", s.accel);
            println!("geometry : {} steps per turntable turn", geo.steps_per_table_rev());
        }
        Cmd::Zero => {
            rig.set_zero()?;
            println!("position set to 0°");
        }
        Cmd::Rotate { degrees } => {
            rig.move_by(geo.degrees_to_steps(degrees))?;
            report(&mut rig)?;
        }
        Cmd::Goto { degrees } => {
            rig.move_to(geo.degrees_to_steps(degrees))?;
            report(&mut rig)?;
        }
        Cmd::Steps { steps } => {
            rig.move_by(steps)?;
            report(&mut rig)?;
        }
        Cmd::Stop => rig.stop()?,
        Cmd::Enable => rig.enable(true)?,
        Cmd::Disable => rig.enable(false)?,
        Cmd::Speed { max_speed, accel } => rig.set_motion(max_speed, accel)?,
        Cmd::Spin { shots, pause_ms } => {
            let start = rig.status()?.position;
            for i in 1..=shots {
                let target = start + geo.degrees_to_steps(360.0 * i as f64 / shots as f64);
                rig.move_to(target)?;
                println!("{i:3}/{shots}  at {:.2}°", geo.steps_to_degrees(target - start));
                sleep(Duration::from_millis(pause_ms));
            }
        }
        Cmd::Listen => {
            println!("press the BOOT button on the board (Ctrl+C to quit)");
            loop {
                if rig.wait_button(Duration::from_secs(1))? {
                    println!("button pressed");
                }
            }
        }
    }
    Ok(())
}
