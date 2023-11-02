use crate::db;
use crate::schema::background_jobs;
use crate::worker::jobs;
use crate::worker::swirl::BackgroundJob;
use anyhow::Result;
use diesel::prelude::*;
use secrecy::{ExposeSecret, SecretString};

#[derive(clap::Parser, Debug)]
#[command(
    name = "enqueue-job",
    about = "Add a job to the background worker queue",
    rename_all = "snake_case"
)]
pub enum Command {
    UpdateDownloads,
    DumpDb {
        #[arg(env = "READ_ONLY_REPLICA_URL")]
        database_url: SecretString,
        #[arg(default_value = "db-dump.tar.gz")]
        target_name: String,
    },
    DailyDbMaintenance,
    SquashIndex,
    NormalizeIndex {
        #[arg(long = "dry-run")]
        dry_run: bool,
    },
}

pub fn run(command: Command) -> Result<()> {
    let conn = &mut db::oneoff_connection()?;
    println!("Enqueueing background job: {command:?}");

    match command {
        Command::UpdateDownloads => {
            let count: i64 = background_jobs::table
                .filter(background_jobs::job_type.eq("update_downloads"))
                .count()
                .get_result(conn)
                .unwrap();

            if count > 0 {
                println!("Did not enqueue update_downloads, existing job already in progress");
                Ok(())
            } else {
                Ok(jobs::UpdateDownloads.enqueue(conn)?)
            }
        }
        Command::DumpDb {
            database_url,
            target_name,
        } => Ok(jobs::DumpDb::new(database_url.expose_secret(), target_name).enqueue(conn)?),
        Command::DailyDbMaintenance => Ok(jobs::DailyDbMaintenance.enqueue(conn)?),
        Command::SquashIndex => Ok(jobs::SquashIndex.enqueue(conn)?),
        Command::NormalizeIndex { dry_run } => {
            Ok(jobs::NormalizeIndex::new(dry_run).enqueue(conn)?)
        }
    }
}
