use std::{
    fs::{self, File},
    path::Path,
};

use clap::CommandFactory;
use clap_mangen::Man;
use remote_luks_unlocker::cli::Args;

fn main() -> std::io::Result<()> {
    let output_path = Path::new("man/remote-luks-unlocker.1");
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut output = File::create(output_path)?;
    let command = Args::command().version(None::<&str>);
    Man::new(command).render(&mut output)
}
