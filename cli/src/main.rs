//! LFFF CLI — LibreFastbootFirmwareFlasher entry point.
//!
//! Subcommands: deps, find-ota, download, extract, devices, arb, flash, flash-partition,
//! reboot, set-slot

mod cli;
mod handlers;
mod output;
mod welcome;

use clap::CommandFactory;
use clap::Parser;
use cli::{Cli, Commands};
use lfff_lib::flasher::FirmwareSource;
use std::process;

fn main() {
    let cli = Cli::parse();

    let log_level = if cli.verbose { "debug" } else { "warn" };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(log_level.parse().unwrap())
                .from_env_lossy(),
        )
        .with_writer(std::io::stderr)
        .without_time()
        .init();

    let exit_code = match cli.command {
        None => {
            welcome::print();
            0
        }
        Some(Commands::Completion { shell }) => handlers::complete::run(&mut Cli::command(), shell),

        Some(Commands::Deps { check, ref tools }) => handlers::deps::run(check, tools),

        Some(Commands::FindOta {
            ref model,
            ref region,
            ref channel,
            latest,
            json,
            changes,
        }) => handlers::find_ota::run(
            model,
            region.as_deref(),
            channel.as_deref(),
            latest,
            json,
            changes,
        ),

        Some(Commands::Download {
            ref url,
            ref output,
            connections,
            ref md5,
        }) => handlers::download::run(url, output.as_ref(), connections, md5.as_deref()),

        Some(Commands::Extract {
            ref zip,
            ref output,
            ref partitions,
            ref checksum,
            list,
        }) => handlers::extract::run(
            zip,
            output.as_ref(),
            partitions.as_deref(),
            checksum.as_deref(),
            list,
        ),

        Some(Commands::Devices { check, ref serial }) => {
            handlers::devices::run(check, serial.as_deref())
        }

        Some(Commands::Arb {
            ref xbl,
            ref firmware_dir,
            device,
            ref serial,
        }) => handlers::arb::run(
            xbl.as_ref(),
            firmware_dir.as_ref(),
            device,
            serial.as_deref(),
        ),

        Some(Commands::Flash {
            ref firmware_dir,
            ref source,
            ref serial,
            ref method,
            dry_run,
            skip_xbl_abl,
            skip_preloader,
            no_rebuild_super,
        }) => {
            // clap requires one of the two (`required_unless_present`).
            let source = match (firmware_dir, source) {
                (Some(dir), _) => FirmwareSource::Extracted(dir.clone()),
                (None, Some(dir)) => FirmwareSource::SourceBuild(dir.clone()),
                (None, None) => unreachable!("clap requires a firmware directory or --source"),
            };
            handlers::flash::run(
                &source,
                serial.as_deref(),
                method.as_deref(),
                dry_run,
                skip_xbl_abl,
                skip_preloader,
                !no_rebuild_super,
            )
        }

        Some(Commands::FlashPartition {
            ref image,
            ref firmware_dir,
            ref partition,
            ref slot,
            no_ab,
            dry_run,
            ref serial,
        }) => handlers::flash_partition::run(
            image,
            firmware_dir.as_ref(),
            partition.as_deref(),
            slot.as_deref(),
            no_ab,
            dry_run,
            serial.as_deref(),
        ),

        Some(Commands::Reboot { to, ref serial }) => handlers::reboot::run(to, serial.as_deref()),

        Some(Commands::SetSlot { slot, ref serial }) => {
            handlers::set_slot::run(slot, serial.as_deref())
        }
    };

    process::exit(exit_code);
}
