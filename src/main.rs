#![allow(clippy::too_many_arguments)]

mod database;
mod driver;
mod errors;
mod queries;
mod time;
mod types;

use std::{
    fs,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

use anyhow::{bail, Context};
use clap::{Parser, Subcommand};
use queries::block::Compression;
use scopeguard::defer;

use crate::database::DatabaseOps;
use crate::driver::FuseDriver;
use simple_logger::SimpleLogger;

#[derive(Parser, Debug)]
struct Cli {
    #[arg(short = 'l', long, default_value = "info")]
    log_level: log::LevelFilter,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Mount the database on a directory.
    Mount {
        #[arg(long = "db", help = "Database file path")]
        database_path: PathBuf,

        #[arg(long = "dest", long = "mount", help = "Path where filesystem will be mounted")]
        mount_path: PathBuf,

        #[clap(flatten)]
        key_group: KeyGroup,
    },
    MountExec {
        #[arg(long = "db", help = "Database file path")]
        database_path: PathBuf,

        #[arg(long = "dest", long = "mount", help = "Path where filesystem will be mounted")]
        mount_path: PathBuf,

        #[clap(flatten)]
        key_group: KeyGroup,

        #[clap(long = "cmd", help = "Command to execute")]
        cmd: String,

        #[clap(long = "arg", short = 'a', help = "Add argument to executed command")]
        args: Vec<String>,
    },
    /// Optimize the database file and reduce disk space usage.
    Optimize {
        #[arg(long = "db", help = "Database file path")]
        database_path: PathBuf,

        #[clap(flatten)]
        key_group: KeyGroup,
    },
}

#[derive(Debug, clap::Args)]
#[group(required = true, multiple = false)]
struct KeyGroup {
    #[arg(long, help = "Decryption key")]
    key: Option<String>,

    #[arg(long, help = "Path to file containing decryption key")]
    key_file: Option<PathBuf>,
}

impl KeyGroup {
    fn read_key(self) -> anyhow::Result<String> {
        let key = if let Some(key) = self.key {
            key
        } else if let Some(key_file) = self.key_file {
            let raw_key = fs::read_to_string(key_file)?;
            raw_key.trim_end().to_owned()
        } else {
            bail!("One of --key or --key-file is required");
        };

        if key.is_empty() {
            bail!("Key cannot be empty");
        }

        Ok(key)
    }
}

fn main() -> anyhow::Result<()> {
    let args = Cli::parse();

    SimpleLogger::new()
        .with_level(args.log_level)
        .init()
        .context("unable to install logging")?;

    match args.command {
        Commands::Mount {
            database_path,
            mount_path,
            key_group,
        } => {
            let db = DatabaseOps::open(&database_path, key_group.read_key()?).context("open db")?;
            let driver = FuseDriver::new(db, Compression::Zstd);

            let mount = fuser::spawn_mount2(driver, &mount_path, &[]).context("unable to create mount")?;
            defer! {
                // Umount & cleanup
                mount.join();
            }

            let term = Arc::new(AtomicBool::new(false));
            signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&term))?;
            signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&term))?;
            while !term.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(100));
            }
        }
        Commands::MountExec {
            database_path,
            mount_path,
            key_group,
            cmd,
            args,
        } => {
            let db = DatabaseOps::open(&database_path, key_group.read_key()?).context("open db")?;
            let driver = FuseDriver::new(db, Compression::Zstd);
            let mount = fuser::spawn_mount2(driver, &mount_path, &[]).context("unable to create mount")?;
            defer! {
                // Umount & cleanup
                mount.join();
            }

            log::info!("Running {:?} with args {:?}", cmd, args);

            let mut child = Command::new(&cmd)
                .args(args)
                .env("NIGHTSHIFT_DB_PATH", database_path)
                .env("NIGHTSHIFT_MOUNT_PATH", mount_path)
                .stdin(Stdio::null())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .context(format!("could not spawn cmd {:?}", cmd))?;

            let status = child.wait()?;
            if !status.success() {
                log::error!("Command exited with status {}", status);
                bail!("Command failure");
            } else {
                log::info!("Command exited with status {}", status);
            }
        }
        Commands::Optimize {
            database_path,
            key_group,
        } => {
            let mut db = DatabaseOps::open(&database_path, key_group.read_key()?).context("open db")?;
            println!("Running VACUUM on database, this may take a few seconds...");
            db.vacuum()?;
            println!("Done!");
        }
    };

    Ok(())
}
