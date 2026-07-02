//! LFFF CLI — LibreFastbootFirmwareFlasher entry point.
//!
//! Subcommands: deps, download, extract, devices, arb, flash, flash-partition

mod cli;
mod handlers;
mod output;
mod welcome;

use std::process;

use clap::Parser;
use cli::{Cli, Commands};
use lfff_lib::flasher::FirmwareSource;

fn main() {
    let cli = Cli::parse();

    let log_level = if cli.verbose { "debug" } else { "warn" };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(log_level.parse().unwrap())
                .from_env_lossy(),
        )
        .without_time()
        .init();

    let exit_code = match cli.command {
        None => {
            welcome::print();
            0
        }

        Some(Commands::Deps { check, ref tools }) => handlers::deps::run(check, tools),

        Some(Commands::Download {
            ref url,
            ref output,
            connections,
        }) => handlers::download::run(url, output.as_ref(), connections),

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

        Some(Commands::Devices { check, ref serial }) => handlers::devices::run(check, serial.as_deref()),

        Some(Commands::Arb {
            ref xbl,
            ref firmware_dir,
        }) => handlers::arb::run(xbl.as_ref(), firmware_dir.as_ref()),

        Some(Commands::Flash {
            ref firmware_dir,
            ref source,
            ref serial,
            ref method,
            dry_run,
            skip_xbl_abl,
            skip_preloader,
        }) => {
            if let Some(dir) = firmware_dir {
                handlers::flash::run(
                    &FirmwareSource::Extracted(dir.clone()),
                    serial.as_deref(),
                    method.as_deref(),
                    dry_run,
                    skip_xbl_abl,
                    skip_preloader,
                )
            } else if let Some(dir) = source {
                handlers::flash::run(
                    &FirmwareSource::SourceBuild(dir.clone()),
                    serial.as_deref(),
                    method.as_deref(),
                    dry_run,
                    skip_xbl_abl,
                    skip_preloader,
                )
            } else {
                println!("✗ Specify a firmware directory or --source DIR");
                eprintln!("Usage: lfff flash <DIR>  or  lfff flash --source <DIR>");
                1
            }
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
            image.as_deref(),
            firmware_dir.as_ref(),
            partition.as_deref(),
            slot.as_deref(),
            no_ab,
            dry_run,
            serial.as_deref(),
        ),
    };

    process::exit(exit_code);
}
