use clap::{Args, ValueHint};
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct ExtractRpuArgs {
    #[arg(
        id = "input",
        help = "Sets the input AV1 file to use",
        long,
        short = 'i',
        conflicts_with = "input_pos",
        required_unless_present = "input_pos",
        value_hint = ValueHint::FilePath,
    )]
    pub input: Option<PathBuf>,

    #[arg(
        id = "input_pos",
        help = "Sets the input AV1 file to use (positional)",
        conflicts_with = "input",
        required_unless_present = "input",
        value_hint = ValueHint::FilePath
    )]
    pub input_pos: Option<PathBuf>,

    #[arg(
        long,
        short = 'o',
        help = "RPU output file location",
        value_hint = ValueHint::FilePath
    )]
    pub rpu_out: Option<PathBuf>,

    #[arg(
        id = "limit",
        long,
        short = 'l',
        help = "Stop processing input after N OBUs"
    )]
    pub limit: Option<u64>,
}
